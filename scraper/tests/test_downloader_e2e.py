from __future__ import annotations

import io
import json
import zipfile

from blake3 import blake3
from conftest import Route

from launcher_scraper.browser import HttpBrowserExecutor, HttpPageFetcher
from launcher_scraper.downloader import HttpDownloader, ScratchBudget
from launcher_scraper.models import DownloadCandidate, SourceDefinition
from launcher_scraper.security import DomainRateLimiter, URLPolicy
from launcher_scraper.service import ScraperService
from launcher_scraper.validation import ArtifactValidator, DedupIndex


def _zip_bytes() -> bytes:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("Game/readme.txt", "fixture")
    return stream.getvalue()


def test_downloader_follows_checked_redirect_and_resumes(fixture_server, tmp_path) -> None:
    body = _zip_bytes()
    fixture_server.routes["/artifact.zip"] = Route(body=body, content_type="application/zip")
    fixture_server.routes["/redirect"] = Route(status=302, redirect="/artifact.zip")
    policy = URLPolicy(allow_localhost=True, resolve_dns=False)
    downloader = HttpDownloader(
        policy=policy, rate_limiter=DomainRateLimiter(), scratch_budget=ScratchBudget(10_000_000)
    )
    candidate = DownloadCandidate(fixture_server.url("/redirect"), "Download", filename="artifact.zip", confidence=1.0)
    part = tmp_path / "out" / ".artifact.zip.part"
    part.parent.mkdir()
    part.write_bytes(body[:8])
    progress: list[tuple[int, int | None]] = []

    result = downloader.download(
        candidate,
        part.parent,
        retry_policy=None,
        minimum_request_interval_seconds=0,
        progress=lambda completed, total: progress.append((completed, total)),
    )
    assert result.final_url == fixture_server.url("/artifact.zip")
    assert result.redirect_chain == (fixture_server.url("/artifact.zip"),)
    assert open(result.path, "rb").read() == body
    assert progress
    assert progress[-1] == (len(body), len(body))


def test_local_fixture_end_to_end_writes_handoff_without_gemini(fixture_server, tmp_path) -> None:
    body = _zip_bytes()
    fixture_server.routes["/release"] = Route(
        body=(
            b"<html><head><title>Fixture Game v1.2.3 Windows x64</title></head>"
            b'<body><h1>Fixture Game 1.2.3</h1><a href="/redirect">Download ZIP</a></body></html>'
        ),
        content_type="text/html",
    )
    fixture_server.routes["/artifact.zip"] = Route(body=body, content_type="application/zip")
    fixture_server.routes["/redirect"] = Route(status=302, redirect="/artifact.zip")
    policy = URLPolicy(allow_localhost=True, resolve_dns=False)
    fetcher = HttpPageFetcher(policy=policy)
    service = ScraperService(
        browser=HttpBrowserExecutor(fetcher=fetcher),
        downloader=HttpDownloader(policy=policy, scratch_budget=ScratchBudget(10_000_000)),
        validator=ArtifactValidator(),
        dedup=DedupIndex(tmp_path / "dedup.json"),
        url_policy=policy,
    )
    source = SourceDefinition("fixture", fixture_server.url("/release"), gemini_fallback_allowed=False)

    outcome = service.ingest(source, tmp_path / "artifacts")

    assert outcome.status.value == "SUCCESS"
    assert outcome.artifact is not None
    assert outcome.artifact.validation.blake3 == blake3(body).hexdigest()
    handoff = json.loads(
        (tmp_path / "artifacts" / "fixture" / outcome.artifact.source_release_id / "handoff.json").read_text()
    )
    assert handoff["normalized_metadata"]["downstream"]["artifact_is_ready"] is True
    assert handoff["release"]["version"] == "1.2.3"
