#!/usr/bin/env python3
"""Harness-only forwarding proxy for the Azure MW2 court (X13/N14).

This is test infrastructure, not a product component. It sits between one
WeftFS writer and the pinned Azurite emulator and does one of three things:

  swallow                 Forward the request, apply it upstream, write the
                          "applied" marker, then withhold the response and keep
                          the client blocked on read. The court's writer has the
                          WEFT_AZURE_AMBIGUOUS_READY fault point armed and kills
                          itself when the marker appears, so the process dies
                          after the conditional head PUT was issued and applied
                          and before the response was consumed.

  ignores-if-match        Drop every If-Match header, i.e. accept a stale
                          conditional write the way a broken endpoint would.
                          A writable open must refuse.

  ignores-if-none-match   Drop every If-None-Match header, i.e. accept a
                          duplicate create-once write. A writable open must
                          refuse.

The Blob SharedKey string-to-sign includes If-Match and If-None-Match, so a
modified request must be re-signed or Azurite would reject it as an auth
failure rather than as an accepted precondition. The proxy therefore re-signs
only the requests it modifies, with the published Azurite development account
key (passed in the environment for the court only). Every other request is
forwarded verbatim with the client's own Authorization header.

The proxy never weakens the real client: the real client talks to it exactly as
it would talk to an endpoint, and its probe is what must fail closed.

Usage:
  azure_mw2_proxy.py --listen 10001 --upstream host:10000 \
      --mode swallow --ready /tmp/ready.json [--target superblock]
"""

import argparse
import base64
import hashlib
import hmac
import http.client
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote


def parse_query(query):
    pairs = []
    if not query:
        return pairs
    for item in query.split("&"):
        if not item:
            continue
        name, _, value = item.partition("=")
        pairs.append((unquote(name), unquote(value)))
    return pairs


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        if os.environ.get("WEFT_PROXY_VERBOSE"):
            sys.stderr.write("proxy: " + (fmt % args) + "\n")

    def do_GET(self):
        self.forward()

    def do_HEAD(self):
        self.forward()

    def do_PUT(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def do_DELETE(self):
        self.forward()

    def upstream_connection(self):
        return http.client.HTTPConnection(
            self.server.upstream_host, self.server.upstream_port, timeout=60
        )

    def resign(self, headers, body_len):
        """Recompute the Blob SharedKey signature for a modified request."""
        key = self.server.account_key
        account = self.server.account
        if not key or not account:
            return headers
        lowered = {name.lower(): value for name, value in headers}
        sends_body = self.command in ("PUT", "POST", "PATCH")
        content_length = ""
        if sends_body and lowered.get("content-length", "0") not in ("", "0"):
            content_length = lowered["content-length"]
        string_to_sign = "\n".join(
            [
                self.command,
                lowered.get("content-encoding", ""),
                lowered.get("content-language", ""),
                content_length,
                lowered.get("content-md5", ""),
                lowered.get("content-type", ""),
                lowered.get("date", ""),
                lowered.get("if-modified-since", ""),
                lowered.get("if-match", ""),
                lowered.get("if-none-match", ""),
                lowered.get("if-unmodified-since", ""),
                lowered.get("range", ""),
            ]
        )
        string_to_sign += "\n"
        canonical_headers = ""
        for name in sorted(key for key in lowered if key.startswith("x-ms-")):
            canonical_headers += f"{name}:{lowered[name].strip()}\n"
        path, _, query = self.path.partition("?")
        resource = "/" + account + unquote(path)
        pairs = parse_query(query)
        for name in sorted({name.lower() for name, _ in pairs}):
            values = sorted(value for n, value in pairs if n.lower() == name)
            resource += "\n" + name + ":" + ",".join(values)
        signature = hmac.new(
            base64.b64decode(key),
            (string_to_sign + canonical_headers + resource).encode(),
            hashlib.sha256,
        ).digest()
        out = [(name, value) for name, value in headers if name.lower() != "authorization"]
        out.append(
            ("Authorization", f"SharedKey {account}:{base64.b64encode(signature).decode()}")
        )
        return out

    def forward(self):
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length) if length else b""
        headers = list(self.headers.items())
        lowered = {name.lower() for name, _ in headers}
        mode = self.server.mode

        is_target_put = (
            self.command == "PUT"
            and self.server.target in self.path
            and "if-match" in lowered
        )
        if self.server.once and self.server.swallowed:
            is_target_put = False

        modified = False
        if mode == "ignores-if-match" and "if-match" in lowered:
            headers = [(n, v) for n, v in headers if n.lower() != "if-match"]
            modified = True
        elif mode == "ignores-if-none-match" and "if-none-match" in lowered:
            headers = [(n, v) for n, v in headers if n.lower() != "if-none-match"]
            modified = True
        if modified:
            headers = self.resign(headers, len(body))

        conn = self.upstream_connection()
        try:
            conn.putrequest(
                self.command,
                self.path,
                skip_host=True,
                skip_accept_encoding=True,
            )
            for name, value in headers:
                conn.putheader(name, value)
            conn.endheaders(body)
            response = conn.getresponse()
            payload = response.read()
            status = response.status
            reason = response.reason
            response_headers = response.getheaders()
        except Exception as exc:  # a broken upstream is reported, never hidden
            self.send_response(502)
            message = f"proxy upstream failure: {exc}".encode()
            self.send_header("Content-Length", str(len(message)))
            self.end_headers()
            self.wfile.write(message)
            conn.close()
            return
        conn.close()

        if os.environ.get("WEFT_PROXY_VERBOSE"):
            sys.stderr.write(
                f"proxy: {self.command} {self.path} -> {status} ({len(payload)}B) "
                f"mode={mode} modified={modified}\n"
            )
            sys.stderr.flush()

        if mode == "swallow" and is_target_put:
            self.server.swallowed = True
            marker = {
                "status": status,
                "method": self.command,
                "path": self.path,
                "response_bytes": len(payload),
                "pid": os.getpid(),
            }
            with open(self.server.ready, "w", encoding="utf-8") as handle:
                json.dump(marker, handle, sort_keys=True)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            # Withhold the response. The writer is blocked reading; the court's
            # fault point kills it. Reading returns when the client socket
            # closes, so this thread does not linger.
            try:
                self.connection.settimeout(60)
                self.connection.recv(1)
            except Exception:
                pass
            self.close_connection = True
            return

        self.send_response(status, reason)
        for name, value in response_headers:
            if name.lower() in (
                "connection",
                "transfer-encoding",
                "content-length",
                "date",
                "server",
            ):
                continue
            self.send_header(name, value)
        if self.command == "HEAD":
            # A HEAD response has no body, but its Content-Length describes the
            # object. Replacing it with the body length (zero) would make a
            # capability probe see a length mismatch and refuse the endpoint.
            upstream_length = None
            for name, value in response_headers:
                if name.lower() == "content-length":
                    upstream_length = value
            if upstream_length is not None:
                self.send_header("Content-Length", upstream_length)
            self.end_headers()
            return
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", type=int, required=True)
    parser.add_argument("--upstream", required=True, help="host:port")
    parser.add_argument(
        "--mode",
        required=True,
        choices=["swallow", "ignores-if-match", "ignores-if-none-match"],
    )
    parser.add_argument("--ready", default="", help="marker path for swallow mode")
    parser.add_argument("--target", default="superblock")
    args = parser.parse_args()

    host, _, port = args.upstream.partition(":")
    server = ThreadingHTTPServer(("127.0.0.1", args.listen), Handler)
    server.upstream_host = host
    server.upstream_port = int(port)
    server.mode = args.mode
    server.ready = args.ready
    server.target = args.target
    server.once = True
    server.swallowed = False
    server.account = os.environ.get("WEFT_AZURE_ACCOUNT", "")
    server.account_key = os.environ.get("WEFT_AZURE_KEY", "")
    server.daemon_threads = True
    print(
        json.dumps(
            {
                "proxy": "listening",
                "listen": args.listen,
                "upstream": args.upstream,
                "mode": args.mode,
                "target": args.target,
            },
            sort_keys=True,
        ),
        flush=True,
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    # Run until the parent closes stdin (the court keeps that pipe open) or the
    # process is terminated.
    try:
        sys.stdin.buffer.read()
    except Exception:
        pass
    server.shutdown()


if __name__ == "__main__":
    main()
