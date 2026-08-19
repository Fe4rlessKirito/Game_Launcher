from __future__ import annotations

import sys
import threading
from collections.abc import Iterator
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

SCRAPER_SRC = Path(__file__).parents[1] / "src"
sys.path.insert(0, str(SCRAPER_SRC))


@dataclass
class Route:
    body: bytes = b""
    status: int = 200
    content_type: str = "application/octet-stream"
    redirect: str | None = None


class _FixtureHandler(BaseHTTPRequestHandler):
    server: FixtureServer

    def do_GET(self) -> None:  # noqa: N802
        route = self.server.routes.get(self.path.split("?", 1)[0])
        if route is None:
            self.send_error(404)
            return
        if route.redirect:
            self.send_response(route.status)
            self.send_header("Location", route.redirect)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        body = route.body
        status = route.status
        content_range = None
        range_header = self.headers.get("Range")
        if range_header and range_header.startswith("bytes=") and status == 200:
            try:
                start = int(range_header.removeprefix("bytes=").split("-", 1)[0])
            except ValueError:
                start = -1
            if 0 <= start < len(body):
                content_range = f"bytes {start}-{len(body) - 1}/{len(body)}"
                body = body[start:]
                status = 206
        self.send_response(status)
        self.send_header("Content-Type", route.content_type)
        self.send_header("Content-Length", str(len(body)))
        if content_range:
            self.send_header("Content-Range", content_range)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_: object) -> None:
        return


class FixtureServer(ThreadingHTTPServer):
    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), _FixtureHandler)
        self.routes: dict[str, Route] = {}
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server_port}"

    def url(self, path: str) -> str:
        return f"{self.base_url}{path}"

    def close(self) -> None:
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)


@pytest.fixture
def fixture_server() -> Iterator[FixtureServer]:
    server = FixtureServer()
    try:
        yield server
    finally:
        server.close()
