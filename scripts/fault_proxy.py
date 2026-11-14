#!/usr/bin/env python3
"""A userspace, fault-injecting HTTP/1.1 forwarding proxy for the T025 court.

It is deliberately small and boring: one request per connection, forwarded to a
real upstream, with a declarative fault schedule. It is **not** a network
emulator, it does not touch the kernel, and every fault it produces is listed in
the receipt. What it can do that a `tc netem` rule cannot is produce a genuine
**ambiguous success**: forward the request, wait for the upstream to apply it,
and then destroy the response (drop it, truncate it, or reset the connection).
The client cannot tell that from a request that never arrived.

Usage:
  fault_proxy.py --listen 127.0.0.1:9100 --upstream http://minio:9000 \
                 --schedule schedule.json --log events.jsonl \
                 [--summary summary.json] [--ready ready.txt]

Schedule:
  {"label": "...",
   "rules": [
     {"method": "PUT", "path_regex": "/superblock$", "occurrence": 1,
      "action": "after_apply_drop"},
     {"method": "PUT", "path_regex": "/objects/", "occurrence": 3,
      "action": "duplicate"},
     {"path_regex": "/superblock$", "occurrence": 2,
      "action": "delay_request:300"}
   ]}

Actions:
  forward                      relay normally (default when no rule matches)
  delay_request:<ms>           wait, then forward and relay
  delay_response:<ms>          forward, wait, then relay
  drop                         close without forwarding
  after_apply_drop             forward, read the full upstream response, send
                               nothing back (ambiguous success)
  after_apply_truncate:<bytes> forward, read the response, send only a prefix,
                               then close (ambiguous success)
  abort_after_apply            forward, read the response, RST the client
                               connection (ambiguous success)
  duplicate                    forward the request twice, relay the first
                               response (at-least-once delivery)

`occurrence` counts matching requests from 1; omit it to match every request.

The proxy never rewrites the request (the S3 client signs the `Host` header, so
rewriting it would invalidate SigV4); it only adds `Connection: close`, which is
not a signed header.
"""

import argparse
import json
import os
import re
import socket
import struct
import sys
import threading
import time

MAX_HEAD = 256 * 1024
MAX_BODY = 512 * 1024 * 1024


def log_line(path, record):
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")


def read_head(sock):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(65536)
        if not chunk:
            return None
        data += chunk
        if len(data) > MAX_HEAD:
            raise ValueError("request head too large")
    head, rest = data.split(b"\r\n\r\n", 1)
    return head, rest


def parse_head(head):
    lines = head.split(b"\r\n")
    request_line = lines[0].decode("latin-1")
    parts = request_line.split(" ")
    if len(parts) != 3:
        raise ValueError(f"malformed request line {request_line!r}")
    method, target, version = parts
    headers = []
    for raw in lines[1:]:
        if not raw:
            continue
        name, _, value = raw.decode("latin-1").partition(":")
        headers.append((name.strip(), value.strip()))
    return method, target, version, headers


def header_value(headers, name):
    wanted = name.lower()
    for key, value in headers:
        if key.lower() == wanted:
            return value
    return None


def read_body(sock, headers, already):
    length_text = header_value(headers, "content-length")
    if length_text is None:
        return already
    length = int(length_text)
    if length > MAX_BODY:
        raise ValueError("request body too large")
    body = bytearray(already)
    while len(body) < length:
        chunk = sock.recv(min(65536, length - len(body)))
        if not chunk:
            raise ValueError("short request body")
        body += chunk
    return bytes(body[:length])


def build_request(method, target, version, headers, body):
    out = [f"{method} {target} {version}"]
    for name, value in headers:
        if name.lower() == "connection":
            continue
        out.append(f"{name}: {value}")
    out.append("Connection: close")
    out.append("")
    out.append("")
    return "\r\n".join(out).encode("latin-1") + body


def forward(upstream_host, upstream_port, request_bytes, timeout=60.0):
    with socket.create_connection((upstream_host, upstream_port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        sock.sendall(request_bytes)
        chunks = []
        while True:
            try:
                chunk = sock.recv(65536)
            except socket.timeout:
                raise
            if not chunk:
                break
            chunks.append(chunk)
        return b"".join(chunks)


def response_status(response):
    try:
        line = response.split(b"\r\n", 1)[0].decode("latin-1")
        return int(line.split(" ")[1])
    except Exception:
        return None


class Proxy:
    def __init__(self, args, rules):
        self.args = args
        self.rules = rules
        self.counts = [0] * len(rules)
        self.lock = threading.Lock()
        self.seq = 0
        self.stats = {"requests": 0, "faults": {}, "bytes_to_client": 0}
        self.upstream_host, self.upstream_port = self.parse_upstream(args.upstream)

    @staticmethod
    def parse_upstream(text):
        if text.startswith("http://"):
            text = text[len("http://"):]
        text = text.rstrip("/")
        if ":" in text:
            host, port = text.rsplit(":", 1)
            return host, int(port)
        return text, 80

    def choose(self, method, target):
        with self.lock:
            self.seq += 1
            seq = self.seq
            for index, rule in enumerate(self.rules):
                if "method" in rule and rule["method"] != method:
                    continue
                pattern = rule.get("path_regex")
                if pattern and not re.search(pattern, target):
                    continue
                self.counts[index] += 1
                occurrence = rule.get("occurrence")
                if occurrence is not None and self.counts[index] != occurrence:
                    continue
                return seq, index, rule.get("action", "forward")
            return seq, None, "forward"

    def handle(self, conn, addr):
        started = time.time()
        record = {
            "ts": time.time(),
            "peer": f"{addr[0]}:{addr[1]}",
            "method": None,
            "path": None,
            "action": "forward",
            "status": None,
            "client_bytes": 0,
            "error": None,
        }
        try:
            conn.settimeout(60.0)
            head = read_head(conn)
            if head is None:
                return
            head_bytes, already = head
            method, target, version, headers = parse_head(head_bytes)
            body = read_body(conn, headers, already)
            record["method"] = method
            record["path"] = target
            seq, rule_index, action = self.choose(method, target)
            record["seq"] = seq
            record["action"] = action
            record["rule"] = rule_index
            request_bytes = build_request(method, target, version, headers, body)
            kind, _, argument = action.partition(":")
            if kind == "drop":
                # The request never reaches the server.
                record["forwarded"] = False
                return
            if kind == "delay_request":
                time.sleep(int(argument) / 1000.0)
            response = b""
            if kind == "duplicate":
                response = forward(self.upstream_host, self.upstream_port, request_bytes)
                try:
                    forward(self.upstream_host, self.upstream_port, request_bytes)
                except OSError as err:
                    record["duplicate_error"] = str(err)
            else:
                response = forward(self.upstream_host, self.upstream_port, request_bytes)
            record["forwarded"] = True
            record["upstream_bytes"] = len(response)
            record["status"] = response_status(response)
            if kind == "delay_response":
                time.sleep(int(argument) / 1000.0)
            if kind == "after_apply_drop":
                # Applied upstream; the client never learns it.
                return
            if kind == "abort_after_apply":
                conn.setsockopt(
                    socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0)
                )
                return
            if kind == "after_apply_truncate":
                limit = int(argument)
                conn.sendall(response[:limit])
                record["client_bytes"] = min(len(response), limit)
                return
            conn.sendall(response)
            record["client_bytes"] = len(response)
        except Exception as err:  # noqa: BLE001 - the proxy must never crash the court
            record["error"] = f"{type(err).__name__}: {err}"
        finally:
            try:
                conn.close()
            except OSError:
                pass
            record["duration_ms"] = round((time.time() - started) * 1000.0, 3)
            with self.lock:
                self.stats["requests"] += 1
                self.stats["bytes_to_client"] += record.get("client_bytes") or 0
                self.stats["faults"][record["action"]] = (
                    self.stats["faults"].get(record["action"], 0) + 1
                )
            log_line(self.args.log, record)

    def serve(self):
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        host, port = self.args.listen.rsplit(":", 1)
        listener.bind((host, int(port)))
        listener.listen(256)
        listener.settimeout(0.5)
        if self.args.ready:
            with open(self.args.ready, "w", encoding="utf-8") as handle:
                handle.write(f"listening {self.args.listen}\n")
        stopping = threading.Event()

        def on_signal(_signum, _frame):
            stopping.set()

        import signal

        signal.signal(signal.SIGTERM, on_signal)
        signal.signal(signal.SIGINT, on_signal)
        while not stopping.is_set():
            try:
                conn, addr = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            thread = threading.Thread(target=self.handle, args=(conn, addr), daemon=True)
            thread.start()
        listener.close()
        if self.args.summary:
            with open(self.args.summary, "w", encoding="utf-8") as handle:
                json.dump(self.stats, handle, indent=2, sort_keys=True)
                handle.write("\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", required=True)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--schedule", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--summary", default=None)
    parser.add_argument("--ready", default=None)
    args = parser.parse_args()
    schedule = json.load(open(args.schedule, encoding="utf-8"))
    rules = schedule.get("rules", [])
    os.makedirs(os.path.dirname(args.log) or ".", exist_ok=True)
    open(args.log, "a", encoding="utf-8").close()
    Proxy(args, rules).serve()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
