from __future__ import annotations

import json

from launcher_scraper.diagnostics import DiagnosticsWriter
from launcher_scraper.interpreter import GeminiPageInterpreter
from launcher_scraper.models import PageSnapshot, ScrapeStatus, SourceDefinition
from launcher_scraper.service import ScrapeOutcome


class _Response:
    def __init__(self, value: dict) -> None:
        self.value = json.dumps(value).encode()

    def __enter__(self) -> _Response:
        return self

    def __exit__(self, *_: object) -> None:
        return

    def read(self, _limit: int = -1) -> bytes:
        return self.value


def test_gemini_discovers_available_model_and_sends_only_semantic_page(monkeypatch) -> None:
    requests = []

    def fake_urlopen(request, timeout):
        del timeout
        requests.append(request)
        if request.get_method() == "GET":
            return _Response(
                {
                    "models": [
                        {"name": "models/unsupported", "supportedGenerationMethods": ["embedContent"]},
                        {"name": "models/fixture-model", "supportedGenerationMethods": ["generateContent"]},
                    ]
                }
            )
        return _Response(
            {
                "candidates": [
                    {
                        "content": {
                            "parts": [
                                {
                                    "text": json.dumps(
                                        {
                                            "action": "EXTRACT_RELEASE",
                                            "target_id": None,
                                            "reason": "release metadata is visible",
                                            "confidence": 0.9,
                                            "wait_seconds": 0,
                                            "scroll_delta": 0,
                                        }
                                    )
                                }
                            ]
                        }
                    }
                ]
            }
        )

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)
    interpreter = GeminiPageInterpreter(api_key="fixture-key")
    page = PageSnapshot("https://example.test/release", "Fixture", visible_text="Download", state_hash="state")

    action = interpreter.decide(page)

    assert action.action.value == "EXTRACT_RELEASE"
    assert interpreter.model == "fixture-model"
    assert len(requests) == 2
    assert b"<html" not in requests[1].data
    assert b"state" in requests[1].data
    assert "fixture-key" not in requests[1].full_url


def test_diagnostics_are_bounded_and_html_free(tmp_path) -> None:
    writer = DiagnosticsWriter(tmp_path, max_bytes=1024)
    source = SourceDefinition("fixture", "https://example.test/release")
    outcome = ScrapeOutcome(
        status=ScrapeStatus.MANUAL_REVIEW,
        message="review required",
        visited_urls=("https://example.test/release",),
        action_history=({"action": "ABORT"},),
    )

    path = writer.write("job-1", source, outcome)

    assert path is not None
    data = json.loads(open(path, encoding="utf-8").read())
    assert data["status"] == "MANUAL_REVIEW"
    assert "<html" not in open(path, encoding="utf-8").read()
