#!/usr/bin/env python3
"""Private streaming proxy for the local Telegram Bot API file paths.

In --local mode getFile returns an absolute path inside the Bot API
container. The restore worker is a separate Railway service, so it cannot
read that path directly. This proxy keeps the Bot API private and exposes
only files below its configured data/temp directories over the same private
service port used by the worker.
"""

from __future__ import annotations

import argparse
import http.client
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlsplit


CHUNK_SIZE = 1024 * 1024


def allowed_file_path(raw_path: str) -> str | None:
    candidate = os.path.realpath(unquote(raw_path))
    roots = [
        os.path.realpath(os.environ.get("TELEGRAM_BOT_API_DIR", "/var/lib/telegram-bot-api")),
        os.path.realpath(os.environ.get("TELEGRAM_BOT_API_TEMP_DIR", "/tmp/telegram-bot-api")),
    ]
    for root in roots:
        if candidate == root or candidate.startswith(root + os.sep):
            return candidate
    return None


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlsplit(self.path)
        if parsed.path.startswith("/file/bot"):
            self.serve_file(parsed.path)
            return
        self.forward(parsed)

    def do_HEAD(self) -> None:  # noqa: N802
        parsed = urlsplit(self.path)
        if parsed.path.startswith("/file/bot"):
            self.send_error(405, "HEAD is not supported for private file paths")
            return
        self.forward(parsed)

    def do_POST(self) -> None:  # noqa: N802
        self.forward(urlsplit(self.path))

    def serve_file(self, path: str) -> None:
        # The local Bot API returns /absolute/path. The worker constructs the
        # standard /file/bot<TOKEN>/<file_path> route, so recover that path
        # without ever logging or validating the bot token in the proxy.
        tail = path[len("/file/bot") :]
        separator = tail.find("/")
        if separator < 0:
            self.send_error(404)
            return
        file_path = "/" + tail[separator + 1 :].lstrip("/")
        resolved = allowed_file_path(file_path)
        if resolved is None or not os.path.isfile(resolved):
            self.send_error(404)
            return
        try:
            size = os.path.getsize(resolved)
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(size))
            self.send_header("Connection", "close")
            self.end_headers()
            with open(resolved, "rb") as source:
                while True:
                    block = source.read(CHUNK_SIZE)
                    if not block:
                        break
                    self.wfile.write(block)
        except (BrokenPipeError, ConnectionResetError):
            return
        except OSError:
            if not self.wfile.closed:
                self.send_error(500)

    def forward(self, parsed) -> None:
        content_length = int(self.headers.get("Content-Length", "0"))
        upstream = http.client.HTTPConnection(
            self.server.upstream_host,
            self.server.upstream_port,
            timeout=300,
        )
        try:
            headers = {
                key: value
                for key, value in self.headers.items()
                if key.lower() not in {"host", "connection", "transfer-encoding"}
            }
            headers["Host"] = f"127.0.0.1:{self.server.upstream_port}"
            headers["Connection"] = "close"
            upstream.putrequest(self.command, parsed.path + (f"?{parsed.query}" if parsed.query else ""))
            for key, value in headers.items():
                upstream.putheader(key, value)
            upstream.endheaders()
            remaining = content_length
            while remaining:
                block = self.rfile.read(min(CHUNK_SIZE, remaining))
                if not block:
                    break
                upstream.send(block)
                remaining -= len(block)
            response = upstream.getresponse()
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() not in {"connection", "transfer-encoding", "server", "date"}:
                    self.send_header(key, value)
            self.send_header("Connection", "close")
            self.end_headers()
            while True:
                block = response.read(CHUNK_SIZE)
                if not block:
                    break
                self.wfile.write(block)
        except (ConnectionError, OSError, http.client.HTTPException):
            if not self.wfile.closed:
                self.send_error(502, "Telegram Bot API is unavailable")
        finally:
            upstream.close()

    def log_message(self, format: str, *args) -> None:
        # Do not log URLs: Bot API URLs contain the bot token.
        return


class ProxyServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, handler, upstream_host: str, upstream_port: int):
        super().__init__(address, handler)
        self.upstream_host = upstream_host
        self.upstream_port = upstream_port


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", type=int, required=True)
    parser.add_argument(
        "--upstream-host",
        default=os.environ.get("TELEGRAM_BOT_API_UPSTREAM_HOST", "127.0.0.1"),
    )
    parser.add_argument("--upstream", type=int, required=True)
    args = parser.parse_args()
    server = ProxyServer(
        ("0.0.0.0", args.listen),
        ProxyHandler,
        args.upstream_host,
        args.upstream,
    )
    server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
