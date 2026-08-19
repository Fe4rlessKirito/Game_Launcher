from __future__ import annotations

from dataclasses import dataclass

import pytest

from launcher_scraper.adapters import AdapterRegistry
from launcher_scraper.interpreter import FakePageInterpreter, PageInterpreterError, _parse_action
from launcher_scraper.models import (
    ElementTarget,
    PageAction,
    PageActionType,
    PageSnapshot,
    SemanticLink,
    SourceDefinition,
)
from launcher_scraper.planner import UnknownSitePlanner


def test_structured_action_parser_rejects_extra_fields_and_bad_targets() -> None:
    valid = {
        "action": "FOLLOW_LINK",
        "target_id": "L1",
        "reason": "release",
        "confidence": 0.9,
        "wait_seconds": 0,
        "scroll_delta": 0,
    }
    assert _parse_action(valid).action == PageActionType.FOLLOW_LINK
    with pytest.raises(PageInterpreterError):
        _parse_action(valid | {"selector": "#download"})
    with pytest.raises(PageInterpreterError):
        _parse_action(valid | {"target_id": None})


@dataclass
class _Session:
    pages: list[PageSnapshot]
    index: int = 0

    @property
    def snapshot(self) -> PageSnapshot:
        return self.pages[self.index]

    def execute(self, action: PageAction) -> PageSnapshot:
        if action.action == PageActionType.FOLLOW_LINK:
            self.index = min(self.index + 1, len(self.pages) - 1)
        return self.snapshot

    def save_download(self, target_id: str, destination: str):  # pragma: no cover - not used by planner
        raise NotImplementedError

    def close(self) -> None:
        return


def _page(url: str, *, link: bool) -> PageSnapshot:
    links = (
        SemanticLink(
            "L1", "Download ZIP" if link else "Latest release", url + "/artifact.zip" if link else url + "/latest"
        ),
    )
    return PageSnapshot(
        url=url,
        title="Fixture Game 1.2.3",
        headings=("Fixture Game 1.2.3",),
        visible_text="Fixture Game 1.2.3",
        links=links,
        downloads_detected=("L1",) if link else (),
        targets=(ElementTarget("L1", "link", 1),),
        state_hash="artifact" if link else "index",
    )


def test_unknown_planner_uses_local_heuristic_before_gemini() -> None:
    first = _page("https://example.test", link=False)
    second = _page("https://example.test/latest", link=True)
    session = _Session([first, second])
    interpreter = FakePageInterpreter([])
    source = SourceDefinition("unknown", first.url, adapter="generic")

    result = UnknownSitePlanner(source, session, interpreter, AdapterRegistry()).run()

    assert result.status.value == "SUCCESS"
    assert result.gemini_calls == 0
    assert result.browser_actions == 1
    assert len(result.releases) == 1


def test_unknown_planner_stops_on_low_confidence_manual_review() -> None:
    page = PageSnapshot("https://example.test", "Game", headings=("Game",), state_hash="same")
    session = _Session([page])
    interpreter = FakePageInterpreter([PageAction(PageActionType.WAIT, reason="uncertain", confidence=0.2)])
    source = SourceDefinition("unknown", page.url)

    result = UnknownSitePlanner(source, session, interpreter, AdapterRegistry()).run()

    assert result.status.value == "MANUAL_REVIEW"
    assert result.gemini_calls == 1
