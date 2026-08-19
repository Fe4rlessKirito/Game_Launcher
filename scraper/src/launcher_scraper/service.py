from __future__ import annotations

import json
import logging
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .adapters import AdapterError, AdapterRegistry, LayoutChanged, SourceAdapter
from .browser import BrowserError, BrowserExecutor, HttpBrowserExecutor, HttpPageFetcher
from .downloader import (
    DownloadError,
    DownloadProgressCallback,
    DownloadResult,
    HttpDownloader,
    ScratchBudget,
    _checksum_matches,
    _hash_file,
)
from .interpreter import GeminiPageInterpreter, PageInterpreter, PageInterpreterError
from .models import (
    DownloadCandidate,
    PageSnapshot,
    PlannerBudget,
    ReleaseCandidate,
    ScrapeStatus,
    SourceDefinition,
    ValidatedArtifact,
)
from .planner import PlannerResult, UnknownSitePlanner
from .security import URLPolicy, safe_filename
from .validation import ArtifactValidator, DedupIndex

logger = logging.getLogger("launcher_scraper")


@dataclass(frozen=True)
class ScrapeOutcome:
    status: ScrapeStatus
    releases: tuple[ReleaseCandidate, ...] = ()
    artifact: ValidatedArtifact | None = None
    planner: PlannerResult | None = None
    message: str = ""
    visited_urls: tuple[str, ...] = ()
    action_history: tuple[dict[str, Any], ...] = ()
    gemini_calls: int = 0
    browser_actions: int = 0


class ScraperService:
    """Orchestrate discovery and acquisition without touching launcher code."""

    def __init__(
        self,
        *,
        adapters: AdapterRegistry | None = None,
        browser: BrowserExecutor | None = None,
        downloader: HttpDownloader | None = None,
        validator: ArtifactValidator | None = None,
        dedup: DedupIndex | None = None,
        url_policy: URLPolicy | None = None,
        planner_budget: PlannerBudget | None = None,
    ) -> None:
        self.adapters = adapters or AdapterRegistry()
        policy = url_policy or URLPolicy()
        self.browser = browser or HttpBrowserExecutor(fetcher=HttpPageFetcher(policy=policy))
        self.downloader = downloader or HttpDownloader(policy=policy, scratch_budget=ScratchBudget(2 * 512 * 1024**3))
        self.validator = validator or ArtifactValidator()
        self.dedup = dedup
        self.planner_budget = planner_budget or PlannerBudget()

    def discover(
        self,
        source: SourceDefinition,
        *,
        interpreter: PageInterpreter | None = None,
        allow_fallback: bool | None = None,
    ) -> ScrapeOutcome:
        session = None
        try:
            session = self.browser.open(source.base_url)
            return self._discover_in_session(source, session, interpreter, allow_fallback)
        except BrowserError as error:
            return ScrapeOutcome(ScrapeStatus.TEMPORARY_FAILURE, message=str(error))
        finally:
            if session is not None:
                session.close()

    def _discover_in_session(
        self,
        source: SourceDefinition,
        session: Any,
        interpreter: PageInterpreter | None,
        allow_fallback: bool | None,
    ) -> ScrapeOutcome:
        adapter = self._adapter_for(source)
        fallback_allowed = source.gemini_fallback_allowed if allow_fallback is None else allow_fallback
        page = session.snapshot
        if not self._challenge(page):
            try:
                releases = tuple(adapter.discover(source, page))
                if self._usable_releases(releases):
                    self._log(
                        "discovery_success", source, adapter.name, page, release_count=len(releases), gemini=False
                    )
                    return ScrapeOutcome(
                        ScrapeStatus.SUCCESS,
                        releases,
                        message="deterministic adapter succeeded",
                        visited_urls=(page.url,),
                    )
            except (AdapterError, LayoutChanged, ValueError) as error:
                self._log("adapter_fallback", source, adapter.name, page, error=str(error))
        else:
            return ScrapeOutcome(
                ScrapeStatus.CHALLENGE_REQUIRED,
                message="anti-bot challenge detected",
                visited_urls=(page.url,),
            )
        if not fallback_allowed:
            return ScrapeOutcome(
                ScrapeStatus.LAYOUT_CHANGED,
                message="deterministic adapter could not identify a release",
                visited_urls=(page.url,),
            )
        planner = self._planner(source, session, interpreter)
        if planner is None:
            return ScrapeOutcome(
                ScrapeStatus.MANUAL_REVIEW,
                message="Gemini fallback is enabled but no interpreter is configured",
                visited_urls=(page.url,),
            )
        result = planner.run()
        self._log(
            "unknown_site_result",
            source,
            adapter.name,
            result.final_page,
            status=result.status.value,
            gemini_calls=result.gemini_calls,
        )
        return ScrapeOutcome(
            result.status,
            result.releases,
            planner=result,
            message=result.message,
            visited_urls=result.visited_urls,
            action_history=result.action_history,
            gemini_calls=result.gemini_calls,
            browser_actions=result.browser_actions,
        )

    def ingest(
        self,
        source: SourceDefinition,
        output_root: str | Path,
        *,
        target_release_id: str | None = None,
        interpreter: PageInterpreter | None = None,
        allow_fallback: bool | None = None,
    ) -> ScrapeOutcome:
        session = None
        try:
            session = self.browser.open(source.base_url)
            discovery = self._discover_in_session(source, session, interpreter, allow_fallback)
            return self.acquire(
                source,
                output_root,
                discovery,
                target_release_id=target_release_id,
                browser_session=session,
            )
        except BrowserError as error:
            return ScrapeOutcome(ScrapeStatus.TEMPORARY_FAILURE, message=str(error))
        finally:
            if session is not None:
                session.close()

    def acquire(
        self,
        source: SourceDefinition,
        output_root: str | Path,
        discovery: ScrapeOutcome,
        *,
        target_release_id: str | None = None,
        browser_session: Any | None = None,
        progress: DownloadProgressCallback | None = None,
    ) -> ScrapeOutcome:
        """Download and validate a discovery result.

        Keeping acquisition separate lets the durable worker persist the
        discovery state before it starts the potentially long download.
        """

        if discovery.status != ScrapeStatus.SUCCESS or not discovery.releases:
            return discovery
        release = self._select_release(discovery.releases, target_release_id)
        if release is None:
            return self._with_metrics(
                discovery,
                ScrapeStatus.NOT_FOUND,
                "requested release was not discovered",
            )
        candidates = self._adapter_for(source).resolve_downloads(source, release)
        if not candidates:
            return self._with_metrics(
                discovery,
                ScrapeStatus.LAYOUT_CHANGED,
                "release has no safe download candidate",
            )
        destination = Path(output_root) / safe_filename(source.name) / safe_filename(release.source_release_id)
        failures: list[str] = []
        last_status = ScrapeStatus.VALIDATION_FAILED
        for candidate in candidates:
            if candidate.requires_browser and (browser_session is None or not candidate.browser_target_id):
                failures.append(f"{candidate.label}: requires a browser download session")
                last_status = ScrapeStatus.MANUAL_REVIEW
                continue
            try:
                if candidate.requires_browser:
                    result = self._browser_download(
                        browser_session,
                        candidate,
                        destination,
                        expected_size=candidate.reported_size or release.reported_size,
                        expected_checksum=candidate.reported_checksum or release.reported_checksum,
                    )
                else:
                    extra_headers = None
                    if browser_session is not None:
                        request_headers = getattr(browser_session, "request_headers", None)
                        if callable(request_headers):
                            extra_headers = request_headers(candidate.url)
                    result = self.downloader.download(
                        candidate,
                        destination,
                        expected_size=candidate.reported_size or release.reported_size,
                        expected_checksum=candidate.reported_checksum or release.reported_checksum,
                        minimum_request_interval_seconds=source.minimum_request_interval_seconds,
                        max_concurrent_requests=source.max_concurrent_requests,
                        extra_headers=extra_headers,
                        progress=progress,
                    )
                validation = self.validator.validate(result.path, candidate, download=result)
            except BrowserError as error:
                last_status = ScrapeStatus.MANUAL_REVIEW
                failures.append(f"{candidate.label}: browser acquisition failed: {error}")
                continue
            except DownloadError as error:
                last_status = (
                    ScrapeStatus.RATE_LIMITED
                    if error.status == 429
                    else ScrapeStatus.TEMPORARY_FAILURE
                    if error.retryable
                    else ScrapeStatus.VALIDATION_FAILED
                )
                failures.append(f"{candidate.label}: {error}")
                if last_status in {ScrapeStatus.RATE_LIMITED, ScrapeStatus.TEMPORARY_FAILURE}:
                    break
                continue
            if not validation.ok:
                failures.append(f"{candidate.label}: {'; '.join(validation.errors)}")
                Path(result.path).unlink(missing_ok=True)
                continue
            duplicate_of = self.dedup.lookup(validation.blake3) if self.dedup else None
            normalized = self._normalized_metadata(source, release, candidate, result, validation, duplicate_of)
            handoff_path = destination / "handoff.json"
            artifact = ValidatedArtifact(
                path=result.path,
                filename=result.filename,
                source=source.name,
                source_release_id=release.source_release_id,
                source_page_url=release.source_page_url,
                download_url=result.final_url,
                validation=validation,
                release=release,
                normalized_metadata=normalized,
                handoff_path=str(handoff_path),
            )
            self._write_handoff(handoff_path, artifact)
            if self.dedup:
                self.dedup.record(
                    validation.blake3, {"path": result.path, "source": source.name, "release": release.to_dict()}
                )
            self._log(
                "ingest_success",
                source,
                "deterministic-or-gemini",
                None,
                bytes=validation.actual_size,
                blake3=validation.blake3,
            )
            return self._with_metrics(
                discovery,
                ScrapeStatus.SUCCESS,
                "validated handoff ready for existing normalizer/packager",
                artifact=artifact,
            )
        return self._with_metrics(discovery, last_status, "; ".join(failures)[:4000])

    @staticmethod
    def _browser_download(
        session: Any,
        candidate: DownloadCandidate,
        destination: Path,
        *,
        expected_size: int | None,
        expected_checksum: str | None,
    ) -> DownloadResult:
        if session is None or not candidate.browser_target_id:
            raise BrowserError("candidate does not have a browser target")
        filename = safe_filename(candidate.filename)
        requested_path = destination / filename
        browser_download = session.save_download(candidate.browser_target_id, str(requested_path))
        actual_path = Path(browser_download.path)
        if actual_path.resolve() != requested_path.resolve() or not actual_path.is_file() or actual_path.is_symlink():
            raise BrowserError("browser download did not stay at the requested artifact path")
        blake3_value, sha256_value = _hash_file(actual_path)
        actual_size = actual_path.stat().st_size
        if expected_size is not None and actual_size != expected_size:
            raise DownloadError(f"browser download size mismatch: expected {expected_size}, got {actual_size}")
        if expected_checksum and not _checksum_matches(expected_checksum, blake3_value, sha256_value):
            raise DownloadError("browser download checksum does not match")
        return DownloadResult(
            path=str(actual_path),
            filename=filename,
            final_url=browser_download.url,
            status=200,
            headers={},
            redirect_chain=(),
            actual_size=actual_size,
            blake3=blake3_value,
            sha256=sha256_value,
        )

    @staticmethod
    def _with_metrics(discovery: ScrapeOutcome, status: ScrapeStatus, message: str, **kwargs: Any) -> ScrapeOutcome:
        return ScrapeOutcome(
            status,
            discovery.releases,
            planner=discovery.planner,
            message=message,
            visited_urls=discovery.visited_urls,
            action_history=discovery.action_history,
            gemini_calls=discovery.gemini_calls,
            browser_actions=discovery.browser_actions,
            **kwargs,
        )

    def _adapter_for(self, source: SourceDefinition) -> SourceAdapter:
        try:
            return self.adapters.get(source)
        except AdapterError:
            return self.adapters.get(SourceDefinition(**{**source.to_dict(), "adapter": "generic"}))

    def _planner(
        self, source: SourceDefinition, session: Any, interpreter: PageInterpreter | None
    ) -> UnknownSitePlanner | None:
        if interpreter is None:
            try:
                interpreter = GeminiPageInterpreter()
            except PageInterpreterError:
                return None
        return UnknownSitePlanner(source, session, interpreter, self.adapters, self.planner_budget)

    @staticmethod
    def _usable_releases(releases: tuple[ReleaseCandidate, ...]) -> bool:
        return bool(
            releases and any(release.best_download and release.best_download.confidence >= 0.55 for release in releases)
        )

    @staticmethod
    def _select_release(
        releases: tuple[ReleaseCandidate, ...], target_release_id: str | None
    ) -> ReleaseCandidate | None:
        if target_release_id:
            return next(
                (
                    release
                    for release in releases
                    if release.source_release_id == target_release_id or release.version == target_release_id
                ),
                None,
            )
        return max(
            releases, key=lambda release: (_version_key(release.version), release.confidence, release.source_release_id)
        )

    @staticmethod
    def _challenge(page: PageSnapshot) -> bool:
        text = f"{page.title} {page.visible_text}".casefold()
        return any(
            token in text
            for token in (
                "captcha",
                "recaptcha",
                "verify you are human",
                "cloudflare challenge",
                "checking your browser",
            )
        )

    @staticmethod
    def _normalized_metadata(
        source: SourceDefinition,
        release: ReleaseCandidate,
        candidate: DownloadCandidate,
        result: Any,
        validation: Any,
        duplicate_of: Any,
    ) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "source": source.name,
            "source_domain": source.domain,
            "source_release_id": release.source_release_id,
            "product_name": release.product_name,
            "normalized_product_name": release.normalized_product_name,
            "version": release.version,
            "release_date": release.release_date,
            "platform": release.platform,
            "architecture": release.architecture,
            "language": release.language,
            "edition": release.edition,
            "source_page_url": release.source_page_url,
            "download_url": result.final_url,
            "reported_size": candidate.reported_size or release.reported_size,
            "reported_checksum": candidate.reported_checksum or release.reported_checksum,
            "discovered_at": release.discovered_at,
            "confidence": release.confidence,
            "validation": validation.to_dict(),
            "duplicate_of": duplicate_of,
            "downstream": {
                "artifact_is_ready": True,
                "normalizer": "launcher-admin ingest <artifact> --output <package-dir>",
                "packager_is_owned_by_existing_rust_pipeline": True,
            },
        }

    @staticmethod
    def _write_handoff(path: Path, artifact: ValidatedArtifact) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(path.suffix + ".tmp")
        temp.write_text(json.dumps(artifact.to_dict(), indent=2, sort_keys=True) + "\n", encoding="utf-8")
        temp.replace(path)

    @staticmethod
    def _log(event: str, source: SourceDefinition, adapter: str, page: PageSnapshot | None, **fields: Any) -> None:
        extra = {"event": event, "source": source.name, "domain": source.domain, "adapter": adapter, **fields}
        if page is not None:
            extra.update({"url": page.url, "state_hash": page.state_hash})
        logger.info(event, extra=extra)


def _version_key(value: str) -> tuple[Any, ...]:
    if value == "unknown":
        return (0,)
    parts = re.split(r"[.+-]", value.lstrip("vV"))
    result: list[Any] = []
    for part in parts:
        result.append(int(part) if part.isdigit() else part.casefold())
    return (1, *result)
