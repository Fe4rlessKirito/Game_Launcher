from __future__ import annotations

import contextlib
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol
from urllib.parse import urljoin, urlparse

from .models import PageAction, PageActionType, PageSnapshot
from .security import URLPolicy
from .semantic import SemanticDomBuilder


class BrowserError(RuntimeError):
    pass


class BrowserExecutionError(BrowserError):
    pass


@dataclass(frozen=True)
class FetchedPage:
    url: str
    status: int
    headers: dict[str, str]
    body: str
    redirects: tuple[str, ...] = ()


@dataclass(frozen=True)
class BrowserDownload:
    path: str
    url: str
    suggested_filename: str | None = None


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_: object) -> None:
        return None


class BrowserSession(Protocol):
    @property
    def snapshot(self) -> PageSnapshot: ...

    def execute(self, action: PageAction) -> PageSnapshot: ...

    def save_download(self, target_id: str, destination: str) -> BrowserDownload: ...

    def request_headers(self, url: str) -> dict[str, str]: ...

    def close(self) -> None: ...


class BrowserExecutor(Protocol):
    def open(self, url: str) -> BrowserSession: ...

    def close(self) -> None: ...


class HttpPageFetcher:
    """Bounded HTTP page fetcher used by deterministic adapters and tests."""

    def __init__(
        self,
        policy: URLPolicy | None = None,
        user_agent: str = "VaultnodeScraper/0.1 (+server-side ingestion)",
        max_page_bytes: int = 2 * 1024 * 1024,
        timeout_seconds: float = 30.0,
    ) -> None:
        self.policy = policy or URLPolicy()
        self.user_agent = user_agent
        self.max_page_bytes = max_page_bytes
        self.timeout_seconds = timeout_seconds
        self._opener = urllib.request.build_opener(_NoRedirect())

    def fetch(self, url: str) -> FetchedPage:
        current = self.policy.validate(url)
        redirects: list[str] = []
        for _ in range(self.policy.max_redirects + 1):
            # Re-resolve immediately before opening each connection. Compose
            # additionally places the scraper on an egress-only network so a
            # rebinding cannot reach the API, worker, database, or Telegram
            # services even if DNS changes after this check.
            current = self.policy.validate(current)
            request = urllib.request.Request(
                current,
                headers={"User-Agent": self.user_agent, "Accept": "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1"},
                method="GET",
            )
            try:
                response = self._opener.open(request, timeout=self.timeout_seconds)
                with response:
                    status = int(getattr(response, "status", 200))
                    headers = {key.casefold(): value for key, value in response.headers.items()}
                    data = response.read(self.max_page_bytes + 1)
                    if len(data) > self.max_page_bytes:
                        raise BrowserError(f"page exceeded {self.max_page_bytes} bytes")
                    return FetchedPage(
                        current, status, headers, data.decode("utf-8", errors="replace"), tuple(redirects)
                    )
            except urllib.error.HTTPError as error:
                if error.code in {301, 302, 303, 307, 308}:
                    location = error.headers.get("Location")
                    if not location:
                        error.close()
                        raise BrowserError(f"redirect {error.code} did not include Location") from error
                    next_url = urljoin(current, location)
                    error.close()
                    self.policy.validate_redirect(current, next_url)
                    redirects.append(next_url)
                    current = next_url
                    continue
                error.close()
                raise BrowserError(f"page returned HTTP {error.code}") from error
            except urllib.error.URLError as error:
                raise BrowserError(f"page request failed: {error.reason}") from error
        raise BrowserError(f"page exceeded {self.policy.max_redirects} redirects")


class HttpBrowserExecutor:
    """Non-JavaScript browser substitute for known deterministic adapters."""

    def __init__(self, fetcher: HttpPageFetcher | None = None, dom: SemanticDomBuilder | None = None) -> None:
        self.fetcher = fetcher or HttpPageFetcher()
        self.dom = dom or SemanticDomBuilder()
        self._sessions: list[HttpBrowserSession] = []

    def open(self, url: str) -> HttpBrowserSession:
        self._sessions = [session for session in self._sessions if not getattr(session, "_closed", False)]
        session = HttpBrowserSession(self.fetcher, self.dom, self.fetcher.fetch(url))
        self._sessions.append(session)
        return session

    def close(self) -> None:
        for session in self._sessions:
            session.close()
        self._sessions.clear()


class HttpBrowserSession:
    def __init__(self, fetcher: HttpPageFetcher, dom: SemanticDomBuilder, first: FetchedPage) -> None:
        self.fetcher = fetcher
        self.dom = dom
        self._history: list[FetchedPage] = [first]
        self._closed = False

    @property
    def fetched(self) -> FetchedPage:
        if self._closed:
            raise BrowserExecutionError("browser session is closed")
        return self._history[-1]

    @property
    def snapshot(self) -> PageSnapshot:
        page = self.fetched
        return self.dom.build(page.body, page.url)

    def execute(self, action: PageAction) -> PageSnapshot:
        if action.action == PageActionType.FOLLOW_LINK:
            link = next((item for item in self.snapshot.links if item.id == action.target_id), None)
            if link is None:
                raise BrowserExecutionError(f"unknown link target {action.target_id}")
            self._history.append(self.fetcher.fetch(link.href))
        elif action.action == PageActionType.GO_BACK:
            if len(self._history) > 1:
                self._history.pop()
        elif action.action == PageActionType.WAIT:
            time.sleep(min(30.0, max(0.0, action.wait_seconds)))
        elif action.action == PageActionType.SCROLL:
            return self.snapshot
        elif action.action in {PageActionType.REQUEST_MORE_CONTEXT, PageActionType.EXTRACT_RELEASE}:
            return self.snapshot
        elif action.action == PageActionType.CLICK:
            raise BrowserExecutionError(
                "HTTP browser cannot execute JavaScript clicks; configure Playwright/CloakBrowser"
            )
        elif action.action == PageActionType.ABORT:
            raise BrowserExecutionError(action.reason or "planner aborted")
        return self.snapshot

    def save_download(self, target_id: str, destination: str) -> BrowserDownload:
        raise BrowserExecutionError("HTTP browser cannot save browser-triggered downloads")

    def request_headers(self, url: str) -> dict[str, str]:
        self.fetcher.policy.validate(url)
        return {}

    def close(self) -> None:
        self._closed = True


class PlaywrightBrowserExecutor:
    """Playwright-compatible browser backend.

    CloakBrowser can be used by pointing ``executable_path`` at its
    Playwright-compatible executable. The scraper only relies on this wrapper,
    so replacing the browser does not affect adapters or planning code.
    """

    def __init__(
        self,
        *,
        headless: bool = True,
        executable_path: str | None = None,
        profile_dir: str | None = None,
        dom: SemanticDomBuilder | None = None,
        policy: URLPolicy | None = None,
        navigation_timeout_ms: int = 30_000,
    ) -> None:
        self.headless = headless
        self.executable_path = executable_path
        self.profile_dir = profile_dir
        self.dom = dom or SemanticDomBuilder()
        self.policy = policy or URLPolicy()
        self.navigation_timeout_ms = navigation_timeout_ms
        self._playwright: Any = None
        self._browser: Any = None
        self._context: Any = None
        self._sessions: list[PlaywrightBrowserSession] = []

    def _ensure_context(self) -> Any:
        if self._context is not None:
            return self._context
        try:
            from playwright.sync_api import sync_playwright
        except ImportError as error:
            raise BrowserError("Playwright is not installed; install launcher-scraper[browser]") from error
        self._playwright = sync_playwright().start()
        browser_type = self._playwright.chromium
        launch_kwargs: dict[str, Any] = {"headless": self.headless}
        if self.executable_path:
            launch_kwargs["executable_path"] = self.executable_path
        if self.profile_dir:
            self._context = browser_type.launch_persistent_context(self.profile_dir, **launch_kwargs)
        else:
            self._browser = browser_type.launch(**launch_kwargs)
            self._context = self._browser.new_context()
        self._context.route("**/*", self._guard_request)
        self._context.set_default_navigation_timeout(self.navigation_timeout_ms)
        return self._context

    def _guard_request(self, route: Any) -> None:
        request_url = route.request.url
        scheme = urlparse(request_url).scheme.casefold()
        if scheme in {"about", "blob", "data"}:
            route.continue_()
            return
        try:
            self.policy.validate(request_url)
        except ValueError:
            route.abort(error_code="blockedbyclient")
            return
        route.continue_()

    def open(self, url: str) -> PlaywrightBrowserSession:
        self.policy.validate(url)
        context = self._ensure_context()
        self._sessions = [session for session in self._sessions if not getattr(session, "_closed", False)]
        page = context.new_page()
        page.goto(url, wait_until="domcontentloaded")
        session = PlaywrightBrowserSession(page, self.dom, self.policy)
        self._sessions.append(session)
        return session

    def close(self) -> None:
        for session in self._sessions:
            session.close()
        self._sessions.clear()
        if self._context is not None:
            with contextlib.suppress(Exception):
                self._context.close()
            self._context = None
        if self._browser is not None:
            with contextlib.suppress(Exception):
                self._browser.close()
            self._browser = None
        if self._playwright is not None:
            with contextlib.suppress(Exception):
                self._playwright.stop()
            self._playwright = None


class PlaywrightBrowserSession:
    def __init__(self, page: Any, dom: SemanticDomBuilder, policy: URLPolicy) -> None:
        self.page = page
        self.dom = dom
        self.policy = policy
        self._closed = False

    @property
    def snapshot(self) -> PageSnapshot:
        if self._closed:
            raise BrowserExecutionError("browser session is closed")
        try:
            current_url = self.policy.validate(self.page.url)
        except ValueError as error:
            raise BrowserExecutionError(f"browser navigated to a blocked URL: {error}") from error
        return self.dom.build(self.page.content(), current_url, self.page.title())

    def _locator(self, target_id: str) -> Any:
        target = self.snapshot.target(target_id)
        if target is None:
            raise BrowserExecutionError(f"unknown browser target {target_id}")
        selector = "a" if target.kind == "link" else "button, [role=button]"
        return self.page.locator(selector).nth(target.ordinal - 1)

    def execute(self, action: PageAction) -> PageSnapshot:
        if action.action == PageActionType.FOLLOW_LINK:
            link = next((item for item in self.snapshot.links if item.id == action.target_id), None)
            if link is None:
                raise BrowserExecutionError(f"unknown link target {action.target_id}")
            self.policy.validate(link.href)
            self.page.goto(link.href, wait_until="domcontentloaded")
        elif action.action == PageActionType.CLICK:
            self._locator(action.target_id or "").click()
            with contextlib.suppress(Exception):
                self.page.wait_for_load_state("domcontentloaded", timeout=10_000)
        elif action.action == PageActionType.SCROLL:
            self.page.mouse.wheel(0, action.scroll_delta or 900)
        elif action.action == PageActionType.WAIT:
            self.page.wait_for_timeout(int(min(30.0, max(0.0, action.wait_seconds)) * 1000))
        elif action.action == PageActionType.GO_BACK:
            self.page.go_back(wait_until="domcontentloaded")
        elif action.action == PageActionType.ABORT:
            raise BrowserExecutionError(action.reason or "planner aborted")
        return self.snapshot

    def save_download(self, target_id: str, destination: str) -> BrowserDownload:
        locator = self._locator(target_id)
        snapshot = self.snapshot
        target = snapshot.target(target_id)
        link = next((item for item in snapshot.links if item.id == target_id), None)
        if target is None or target.kind != "link" or link is None:
            raise BrowserExecutionError("browser downloads require a link target")
        self.policy.validate(link.href)
        destination_path = Path(destination)
        destination_path.parent.mkdir(parents=True, exist_ok=True)
        with self.page.expect_download(timeout=30_000) as download_info:
            locator.click()
        download = download_info.value
        download.save_as(str(destination_path))
        return BrowserDownload(str(destination_path), download.url, download.suggested_filename)

    def request_headers(self, url: str) -> dict[str, str]:
        self.policy.validate(url)
        cookies = self.page.context.cookies(url)
        cookie_header = "; ".join(f"{item['name']}={item['value']}" for item in cookies if item.get("name"))
        return {"Cookie": cookie_header} if cookie_header else {}

    def close(self) -> None:
        if not self._closed:
            with contextlib.suppress(Exception):
                self.page.close()
            self._closed = True


CloakBrowserExecutor = PlaywrightBrowserExecutor
