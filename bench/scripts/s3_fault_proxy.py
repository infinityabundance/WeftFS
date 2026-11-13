#!/usr/bin/env python3
"""Byte-level provider-fault proxy for the N04 object-pipeline court.

This is harness-only test infrastructure, not a product component. It sits
between one WeftFS writer and a real S3-compatible endpoint and forwards the
exact bytes, so the client's SigV4 signature and every header reach the server
verbatim. It does not re-sign or rewrite anything.

Three modes, one fault target each (the K-th HTTP request whose method is
`PUT`), and the fault fires **once** so the same run can retry:

  count    forward everything; log each request. Used to learn how many PUTs a
           store initialisation plus a small pipeline write produces.
  reset    drop the connection at the target PUT **before forwarding it**: the
           provider never applies that write. This is a provider abort.
  swallow  forward the target PUT, then close the client connection as soon as
           upstream starts answering, so the write is applied but its
           acknowledgement is lost.

The client must fail closed in both fault modes. The court then reopens the
store (must be at the previous head, `verify --full` clean) and retries the
identical write, which must reach the same head as an unfaulted reference run.

Usage:
  s3_fault_proxy.py --listen 10001 --upstream 10.213.77.10:9000 \
      --mode reset --target-put 12 --log /tmp/proxy.jsonl
"""

import argparse
import json
import socket
import socketserver
import sys
import threading

LOG_LOCK = threading.Lock()


class State:
    def __init__(self, args):
        self.args = args
        self.put_count = 0
        self.faulted = False
        self.lock = threading.Lock()

    def next_put(self, method):
        if method != "PUT":
            return 0
        with self.lock:
            self.put_count += 1
            return self.put_count

    def claim_fault(self, method):
        """Return True exactly once, for the target PUT."""
        if method != "PUT" or self.args.mode == "count":
            return False
        with self.lock:
            if self.faulted:
                return False
            if self.put_count != self.args.target_put:
                return False
            self.faulted = True
            return True


def log(state, record):
    record = dict(record)
    record["mode"] = state.args.mode
    with LOG_LOCK:
        state.args.log.write(json.dumps(record, sort_keys=True) + "\n")
        state.args.log.flush()


def parse_request(buf):
    """Parse one request prefix. Returns (method, path, total_len) or None."""
    end = buf.find(b"\r\n\r\n")
    if end < 0:
        return None
    head = buf[:end].decode("latin-1", "replace")
    lines = head.split("\r\n")
    parts = lines[0].split(" ")
    if len(parts) < 2:
        return None
    method, path = parts[0], parts[1]
    length = 0
    chunked = False
    for line in lines[1:]:
        name, _, value = line.partition(":")
        name = name.strip().lower()
        if name == "content-length":
            try:
                length = int(value.strip())
            except ValueError:
                length = 0
        elif name == "transfer-encoding" and "chunked" in value.lower():
            chunked = True
    if chunked:
        # The court's S3 client always sends a byte slice with Content-Length.
        # Refuse to guess: forward this request uninspected and do not count it.
        return method, path, None
    return method, path, end + 4 + length


def handle_client(client, state):
    upstream = socket.create_connection(
        (state.args.upstream_host, state.args.upstream_port), timeout=30
    )
    client.settimeout(60)
    upstream.settimeout(60)

    def close_all():
        for sock in (client, upstream):
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                sock.close()
            except OSError:
                pass

    # client -> upstream, with request framing so the fault targets a real PUT.
    def pump_up():
        buf = b""
        try:
            while True:
                chunk = client.recv(65536)
                if not chunk:
                    break
                buf += chunk
                while True:
                    parsed = parse_request(buf)
                    if parsed is None:
                        break
                    method, path, total = parsed
                    if total is None:
                        # Unframed request: forward what we have and stop
                        # inspecting this connection rather than guess.
                        upstream.sendall(buf)
                        buf = b""
                        break
                    if len(buf) < total:
                        break
                    body = buf[:total]
                    buf = buf[total:]
                    number = state.next_put(method)
                    if number:
                        log(
                            state,
                            {
                                "event": "put",
                                "n": number,
                                "path": path,
                                "bytes": total,
                            },
                        )
                    if state.claim_fault(method):
                        log(
                            state,
                            {
                                "event": "fault",
                                "mode": state.args.mode,
                                "n": number,
                                "path": path,
                                "bytes": total,
                            },
                        )
                        if state.args.mode == "reset":
                            close_all()
                            return
                        upstream.sendall(body)
                        # swallow: the provider applies the write; the next
                        # upstream byte closes the client instead of being
                        # relayed, so the acknowledgement is lost.
                        with state.swallow_lock:
                            state.pending_swallow = True
                        continue
                    upstream.sendall(body)
                if not buf:
                    continue
                # A partial request (headers or body still arriving): keep
                # buffering and wait for the rest.
                if len(buf) > 64 * 1024 * 1024:
                    upstream.sendall(buf)
                    buf = b""
        except OSError:
            pass
        finally:
            try:
                upstream.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    state.pending_swallow = False
    state.swallow_lock = threading.Lock()

    def pump_down():
        try:
            while True:
                chunk = upstream.recv(65536)
                if not chunk:
                    break
                with state.swallow_lock:
                    swallow = state.pending_swallow
                    state.pending_swallow = False
                if swallow:
                    log(state, {"event": "swallow", "mode": state.args.mode})
                    close_all()
                    return
                client.sendall(chunk)
        except OSError:
            pass
        finally:
            try:
                client.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    up = threading.Thread(target=pump_up, daemon=True)
    down = threading.Thread(target=pump_down, daemon=True)
    up.start()
    down.start()
    up.join()
    down.join()
    close_all()


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen", type=int, required=True)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--mode", choices=["count", "reset", "swallow"], required=True)
    parser.add_argument("--target-put", type=int, default=12)
    parser.add_argument("--log", required=True)
    args = parser.parse_args(argv)
    host, _, port = args.upstream.rpartition(":")
    args.upstream_host = host
    args.upstream_port = int(port)
    args.log = open(args.log, "w", encoding="utf-8")
    state = State(args)

    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            handle_client(self.request, state)

    class Proxy(Server):
        pass

    Proxy.RequestHandlerClass = Handler
    server = Proxy(("127.0.0.1", args.listen), Handler)
    print(
        f"s3_fault_proxy: mode={args.mode} target_put={args.target_put} "
        f"upstream={args.upstream} listen={args.listen}",
        file=sys.stderr,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        state.args.log.close()
        server.server_close()


if __name__ == "__main__":
    main()
