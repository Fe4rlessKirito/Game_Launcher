from __future__ import annotations

import json
import re
import time
from dataclasses import dataclass
from typing import Any

from .adapters import AdapterRegistry, SourceAdapter
from .browser import BrowserError, BrowserSession
from .interpreter import PageInterpreter, PageInterpreterError
from .models import (
    PageAction,
    PageActionType,
    PageSnapshot,
    PlannerBudget,
    ReleaseCandidate,
    ScrapeStatus,
    SourceDefinition,
)

_CHALLENGE_RE = re.compile(
    r"\b(captcha|recaptcha|verify you are human|checking your browser|cloudflare challenge|turnstile)\b", re.I
)
_LATEST_RE = re.compile(r"\b(latest|newest|current|release|version|download)\b", re.I)


@dataclass(frozen=True)
class PlannerResult:
    status: ScrapeStatus
    releases: tuple[ReleaseCandidate, ...] = ()
    final_page: PageSnapshot | None = None
    visited_urls: tuple[str, ...] = ()
    action_history: tuple[dict[str, Any], ...] = ()
    gemini_calls: int = 0
    browser_actions: int = 0
    message: str = ""


def _heuristic_action(page: PageSnapshot) -> PageAction | None:
    if page.downloads_detected:
        link = next((link for link in page.links if link.id in page.downloads_detected), None)
        if link is not None:
            return PageAction(
                PageActionType.FOLLOW_LINK,
                link.id,
                "local heuristic selected a semantic download link",
                0.82,
            )
    for link in page.links:
        text = f"{link.text} {link.href}"
        if _LATEST_RE.search(text) and link.id not in page.pagination:
            return PageAction(PageActionType.FOLLOW_LINK, link.id, "local heuristic selected a release link", 0.7)
    if page.headings or page.title:
        return PageAction(
            PageActionType.EXTRACT_RELEASE, reason="page has release-like title or headings", confidence=0.65
        )
    return None


class UnknownSitePlanner:
    """Bounded action loop for pages not covered by deterministic adapters."""

    def __init__(
        self,
        source: SourceDefinition,
        session: BrowserSession,
        interpreter: PageInterpreter,
        adapters: AdapterRegistry,
        budget: PlannerBudget | None = None,
    ) -> None:
        self.source = source
        self.session = session
        self.interpreter = interpreter
        self.adapters = adapters
        self.budget = budget or PlannerBudget()

    def run(self) -> PlannerResult:
        started = time.monotonic()
        visited_urls: list[str] = []
        action_history: list[dict[str, Any]] = []
        seen_states: dict[str, int] = {}
        pages = 0
        total_actions = 0
        gemini_calls = 0
        browser_actions = 0
        depth = 0
        context: str | None = None
        last_page: PageSnapshot | None = None
        adapter = self._generic_adapter()

        while pages < self.budget.max_pages_per_job and time.monotonic() - started < self.budget.max_runtime_seconds:
            page = self.session.snapshot
            last_page = page
            pages += 1
            visited_urls.append(page.url)
            seen_states[page.state_hash] = seen_states.get(page.state_hash, 0) + 1
            if _CHALLENGE_RE.search(f"{page.title} {page.visible_text}"):
                return self._result(
                    ScrapeStatus.CHALLENGE_REQUIRED,
                    page,
                    visited_urls,
                    action_history,
                    gemini_calls,
                    browser_actions,
                    "anti-bot challenge detected; manual review or retry is required",
                )
            if seen_states[page.state_hash] > 1:
                return self._result(
                    ScrapeStatus.LAYOUT_CHANGED,
                    page,
                    visited_urls,
                    action_history,
                    gemini_calls,
                    browser_actions,
                    "planner reached the same semantic page state repeatedly",
                )

            releases = self._extract(adapter, page)
            if releases and any(release.download_candidates for release in releases):
                return self._result(
                    ScrapeStatus.SUCCESS,
                    page,
                    visited_urls,
                    action_history,
                    gemini_calls,
                    browser_actions,
                    "release extracted",
                    releases,
                )

            page_actions = 0
            while page_actions < self.budget.max_actions_per_page and total_actions < self.budget.max_total_actions:
                action = _heuristic_action(page)
                has_download = any(release.download_candidates for release in releases)
                if action is None or (action.action == PageActionType.EXTRACT_RELEASE and not has_download):
                    if gemini_calls >= self.budget.max_gemini_calls:
                        return self._result(
                            ScrapeStatus.LAYOUT_CHANGED,
                            page,
                            visited_urls,
                            action_history,
                            gemini_calls,
                            browser_actions,
                            "deterministic heuristics could not continue within the Gemini budget",
                        )
                    try:
                        action = self.interpreter.decide(page, context)
                    except PageInterpreterError as error:
                        return self._result(
                            ScrapeStatus.TEMPORARY_FAILURE,
                            page,
                            visited_urls,
                            action_history,
                            gemini_calls + 1,
                            browser_actions,
                            str(error),
                        )
                    gemini_calls += 1
                if action.confidence < 0.5 and action.action not in {
                    PageActionType.ABORT,
                    PageActionType.REQUEST_MORE_CONTEXT,
                }:
                    return self._result(
                        ScrapeStatus.MANUAL_REVIEW,
                        page,
                        visited_urls,
                        action_history,
                        gemini_calls,
                        browser_actions,
                        "planner action confidence was below the safe threshold",
                    )
                if action.target_id and action.target_id not in page.target_ids():
                    return self._result(
                        ScrapeStatus.LAYOUT_CHANGED,
                        page,
                        visited_urls,
                        action_history,
                        gemini_calls,
                        browser_actions,
                        f"planner selected unavailable target {action.target_id}",
                    )
                action_history.append({"url": page.url, "state_hash": page.state_hash, **action.to_dict()})
                total_actions += 1
                page_actions += 1
                if action.action == PageActionType.EXTRACT_RELEASE:
                    releases = self._extract(adapter, page)
                    if releases and any(release.download_candidates for release in releases):
                        return self._result(
                            ScrapeStatus.SUCCESS,
                            page,
                            visited_urls,
                            action_history,
                            gemini_calls,
                            browser_actions,
                            "release extracted",
                            releases,
                        )
                    context = json.dumps(page.compact_dict(visible_text_limit=16_000), sort_keys=True)
                    continue
                if action.action == PageActionType.REQUEST_MORE_CONTEXT:
                    context = json.dumps(page.compact_dict(visible_text_limit=16_000), sort_keys=True)
                    continue
                if action.action == PageActionType.ABORT:
                    return self._result(
                        ScrapeStatus.MANUAL_REVIEW,
                        page,
                        visited_urls,
                        action_history,
                        gemini_calls,
                        browser_actions,
                        action.reason or "planner aborted",
                    )
                if action.action in {PageActionType.FOLLOW_LINK, PageActionType.CLICK}:
                    depth += 1
                    if depth > self.budget.max_navigation_depth:
                        return self._result(
                            ScrapeStatus.LAYOUT_CHANGED,
                            page,
                            visited_urls,
                            action_history,
                            gemini_calls,
                            browser_actions,
                            "maximum navigation depth reached",
                        )
                try:
                    page = self.session.execute(action)
                    browser_actions += 1
                except BrowserError as error:
                    return self._result(
                        ScrapeStatus.TEMPORARY_FAILURE,
                        page,
                        visited_urls,
                        action_history,
                        gemini_calls,
                        browser_actions,
                        str(error),
                    )
                break
            else:
                return self._result(
                    ScrapeStatus.LAYOUT_CHANGED,
                    page,
                    visited_urls,
                    action_history,
                    gemini_calls,
                    browser_actions,
                    "page action budget exhausted",
                )
        status = (
            ScrapeStatus.TEMPORARY_FAILURE
            if time.monotonic() - started >= self.budget.max_runtime_seconds
            else ScrapeStatus.LAYOUT_CHANGED
        )
        return self._result(
            status,
            last_page,
            visited_urls,
            action_history,
            gemini_calls,
            browser_actions,
            "planner budget exhausted",
        )

    def _generic_adapter(self) -> SourceAdapter:
        try:
            return self.adapters.get(SourceDefinition(**{**self.source.to_dict(), "adapter": "generic"}))
        except Exception:
            return self.adapters.get(self.source)

    def _extract(self, adapter: SourceAdapter, page: PageSnapshot) -> list[ReleaseCandidate]:
        try:
            return adapter.discover(self.source, page)
        except Exception:
            return []

    @staticmethod
    def _result(
        status: ScrapeStatus,
        page: PageSnapshot | None,
        visited_urls: list[str],
        action_history: list[dict[str, Any]],
        gemini_calls: int,
        browser_actions: int,
        message: str,
        releases: list[ReleaseCandidate] | None = None,
    ) -> PlannerResult:
        return PlannerResult(
            status=status,
            releases=tuple(releases or ()),
            final_page=page,
            visited_urls=tuple(dict.fromkeys(visited_urls)),
            action_history=tuple(action_history),
            gemini_calls=gemini_calls,
            browser_actions=browser_actions,
            message=message,
        )
