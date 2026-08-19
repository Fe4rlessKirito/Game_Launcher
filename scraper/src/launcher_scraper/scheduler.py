from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from pathlib import Path
from threading import Event

from .diagnostics import DiagnosticsWriter
from .interpreter import PageInterpreter
from .jobs import SQLiteJobStore
from .models import JobStatus, ScrapeJob, ScrapeStatus, SourceDefinition
from .service import ScrapeOutcome, ScraperService
from .status import WorkStatusPublisher

logger = logging.getLogger("launcher_scraper.scheduler")


@dataclass(frozen=True)
class WorkerResult:
    job: ScrapeJob
    outcome: ScrapeOutcome


class IngestionScheduler:
    """Durable source scheduler and lease-based scraper worker."""

    def __init__(
        self,
        store: SQLiteJobStore,
        service: ScraperService,
        *,
        worker_id: str = "scraper-worker",
        lease_seconds: int = 300,
        output_root: str | Path = "./scraper-artifacts",
        diagnostics: DiagnosticsWriter | None = None,
        status: WorkStatusPublisher | None = None,
    ) -> None:
        self.store = store
        self.service = service
        self.worker_id = worker_id
        self.lease_seconds = max(30, lease_seconds)
        self.output_root = str(output_root)
        self.diagnostics = diagnostics
        self.status = status

    def register_source(self, source: SourceDefinition) -> None:
        self.store.add_source(source)

    def enqueue_due_sources(self, now: float | None = None) -> list[ScrapeJob]:
        """Create at most one active job per due source and advance its timer."""

        jobs: list[ScrapeJob] = []
        checked_at = time.time() if now is None else now
        for source in self.store.due_sources(checked_at):
            job = self.store.enqueue(source.name)
            jobs.append(job)
            if job.status in {JobStatus.QUEUED, JobStatus.RETRY}:
                self._publish(
                    job,
                    kind="SCRAPER",
                    state="QUEUED",
                    source=source,
                    detail="Waiting for scraper worker",
                )
            self.store.mark_source_checked(source.name, checked_at + source.check_interval_seconds)
        return jobs

    def run_once(self, *, interpreter: PageInterpreter | None = None) -> WorkerResult | None:
        self.store.recover_expired()
        self.enqueue_due_sources()
        job = self.store.claim(self.worker_id, self.lease_seconds)
        if job is None:
            return None
        source = self.store.get_source(job.source_name)
        if source is None:
            outcome = ScrapeOutcome(
                ScrapeStatus.PERMANENT_FAILURE,
                message=f"source {job.source_name!r} no longer exists",
            )
            self._finish(job, outcome)
            return WorkerResult(job, outcome)
        try:
            self._publish(
                job,
                kind="SCRAPER",
                state="DISCOVERING",
                source=source,
                detail="Checking the release source",
            )
            discovery = self.service.discover(source, interpreter=interpreter)
            self._record(job, discovery)
            if discovery.status == ScrapeStatus.SUCCESS:
                job.status = JobStatus.ACQUIRING
                job.stage = "ACQUIRING"
                self.store.heartbeat(job, self.lease_seconds)
                release = discovery.releases[0] if discovery.releases else None
                self._publish(
                    job,
                    kind="SCRAPER",
                    state="DOWNLOADING",
                    source=source,
                    game=release.product_name if release else None,
                    version=release.version if release else None,
                    detail="Downloading and validating the release",
                    bytes_completed=0,
                    bytes_total=release.reported_size if release else None,
                )
                last_progress_at = 0.0
                last_progress_bytes = -1

                def publish_download_progress(bytes_completed: int, bytes_total: int | None) -> None:
                    nonlocal last_progress_at, last_progress_bytes
                    now = time.monotonic()
                    is_final = bytes_total is not None and bytes_completed >= bytes_total
                    if (
                        not is_final
                        and now - last_progress_at < 0.75
                        and bytes_completed - last_progress_bytes < 4 * 1024 * 1024
                    ):
                        return
                    progress_percent = (
                        None
                        if bytes_total is None or bytes_total <= 0
                        else (bytes_completed / bytes_total) * 100
                    )
                    elapsed = now - last_progress_at
                    rate = (
                        None
                        if last_progress_bytes < 0 or elapsed <= 0
                        else int(max(0, (bytes_completed - last_progress_bytes) / elapsed))
                    )
                    self._publish(
                        job,
                        kind="SCRAPER",
                        state="DOWNLOADING",
                        source=source,
                        game=release.product_name if release else None,
                        version=release.version if release else None,
                        detail="Downloading and validating the release",
                        progress_percent=progress_percent,
                        bytes_completed=bytes_completed,
                        bytes_total=bytes_total,
                        rate_bytes_per_second=rate,
                    )
                    last_progress_at = now
                    last_progress_bytes = bytes_completed

                outcome = self.service.acquire(
                    source,
                    self._output_root(),
                    discovery,
                    target_release_id=job.target_release_id,
                    progress=publish_download_progress,
                )
            else:
                outcome = discovery
            self._finish(job, outcome, source)
            return WorkerResult(job, outcome)
        except Exception as error:  # noqa: BLE001 - worker boundary must persist unexpected failures
            logger.exception("scraper job failed unexpectedly", extra={"job_id": job.id, "source": source.name})
            outcome = ScrapeOutcome(ScrapeStatus.TEMPORARY_FAILURE, message=f"unexpected scraper failure: {error}")
            self._finish(job, outcome, source)
            return WorkerResult(job, outcome)

    def run_forever(
        self,
        *,
        poll_seconds: float = 5.0,
        stop_event: Event | None = None,
        interpreter: PageInterpreter | None = None,
    ) -> None:
        stop_event = stop_event or Event()
        while not stop_event.is_set():
            result = self.run_once(interpreter=interpreter)
            if result is None:
                stop_event.wait(max(0.1, poll_seconds))

    def _output_root(self) -> str:
        return self.output_root

    def _record(self, job: ScrapeJob, outcome: ScrapeOutcome) -> None:
        job.visited_urls = list(dict.fromkeys(outcome.visited_urls))
        job.action_history = list(outcome.action_history)
        job.gemini_calls = outcome.gemini_calls
        job.browser_actions = outcome.browser_actions
        job.result_status = outcome.status
        job.last_error = None if outcome.status == ScrapeStatus.SUCCESS else outcome.message[:4000]
        self.store.heartbeat(job, self.lease_seconds)

    def _finish(self, job: ScrapeJob, outcome: ScrapeOutcome, source: SourceDefinition | None = None) -> None:
        self._record(job, outcome)
        if outcome.artifact is not None:
            job.resolved_artifact = outcome.artifact.to_dict()
        if self.diagnostics and source is not None:
            try:
                self.diagnostics.write(job.id, source, outcome)
            except OSError:
                logger.exception("could not write scraper diagnostics", extra={"job_id": job.id})
        if outcome.status == ScrapeStatus.SUCCESS:
            self.store.complete(job)
            self._clear(job)
            return
        retry = outcome.status in {ScrapeStatus.RATE_LIMITED, ScrapeStatus.TEMPORARY_FAILURE}
        self.store.fail(job, outcome.message or outcome.status.value, retry=retry)
        self._clear(job)

    def _publish(
        self,
        job: ScrapeJob,
        *,
        kind: str,
        state: str,
        source: SourceDefinition | None = None,
        game: str | None = None,
        version: str | None = None,
        provider: str | None = None,
        detail: str,
        progress_percent: float | None = None,
        bytes_completed: int | None = None,
        bytes_total: int | None = None,
        rate_bytes_per_second: int | None = None,
    ) -> None:
        if self.status is None:
            return
        if game is None and source is not None:
            metadata_name = source.metadata.get("product_name")
            game = metadata_name if isinstance(metadata_name, str) and metadata_name.strip() else source.name
        try:
            self.status.publish(
                job.id,
                kind=kind,
                state=state,
                game=game,
                version=version,
                provider=provider,
                source=source.name if source is not None else None,
                detail=detail,
                progress_percent=progress_percent,
                bytes_completed=bytes_completed,
                bytes_total=bytes_total,
                rate_bytes_per_second=rate_bytes_per_second,
            )
        except (OSError, ValueError):
            logger.exception("could not publish scraper work status", extra={"job_id": job.id})

    def _clear(self, job: ScrapeJob) -> None:
        if self.status is None:
            return
        try:
            self.status.clear(job.id)
        except (OSError, ValueError):
            logger.exception("could not clear scraper work status", extra={"job_id": job.id})
