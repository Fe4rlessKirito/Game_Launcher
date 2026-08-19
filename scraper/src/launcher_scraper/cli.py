from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import uuid
from pathlib import Path
from typing import Any

from .adapters import AdapterRegistry
from .browser import BrowserExecutor, HttpBrowserExecutor, HttpPageFetcher, PlaywrightBrowserExecutor
from .diagnostics import DiagnosticsWriter
from .downloader import HttpDownloader, ScratchBudget
from .jobs import JobStoreError, SQLiteJobStore
from .models import DownloadCandidate, JobStatus, PlannerBudget, ScrapeStatus, SourceDefinition
from .scheduler import IngestionScheduler
from .security import URLPolicy
from .service import ScrapeOutcome, ScraperService
from .validation import ArtifactValidator, DedupIndex, ValidationLimits

logger = logging.getLogger("launcher_scraper")


def _env_bool(name: str, default: bool = False) -> bool:
    value = os.environ.get(name)
    if value is None:
        return default
    return value.casefold() in {"1", "true", "yes", "on"}


def _env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, str(default)))
    except ValueError as error:
        raise ValueError(f"{name} must be an integer") from error


def _env_float(name: str, default: float) -> float:
    try:
        return float(os.environ.get(name, str(default)))
    except ValueError as error:
        raise ValueError(f"{name} must be a number") from error


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Bounded Vaultnode release scraper")
    parser.add_argument(
        "--store", default=os.environ.get("SCRAPER_STATE_DB", "scraper-state.db"), help="SQLite scraper state database"
    )
    parser.add_argument("--output-root", default=os.environ.get("SCRAPER_OUTPUT_DIR", "scraper-artifacts"))
    parser.add_argument(
        "--dedup", default=os.environ.get("SCRAPER_DEDUP_INDEX", ""), help="optional durable BLAKE3 index"
    )
    parser.add_argument("--browser", choices=("http", "playwright"), default=os.environ.get("SCRAPER_BROWSER", "http"))
    parser.add_argument(
        "--allow-localhost", action="store_true", help="allow loopback URLs for local fixture tests only"
    )
    parser.add_argument("--headful", action="store_true", help="show the Playwright browser")
    parser.add_argument("--playwright-executable", default=os.environ.get("SCRAPER_BROWSER_EXECUTABLE", ""))
    parser.add_argument("--profile-dir", default=os.environ.get("SCRAPER_BROWSER_PROFILE", ""))
    parser.add_argument("--log-level", default=os.environ.get("SCRAPER_LOG_LEVEL", "INFO"))
    subparsers = parser.add_subparsers(dest="command", required=True)

    source_add = subparsers.add_parser("source-add", help="register or update a release source")
    source_add.add_argument("name")
    source_add.add_argument("base_url")
    source_add.add_argument("--adapter", default="generic")
    source_add.add_argument("--check-interval", type=int, default=3600)
    source_add.add_argument("--platform", action="append")
    source_add.add_argument("--language", action="append", default=[])
    source_add.add_argument("--min-request-interval", type=float, default=1.0)
    source_add.add_argument("--max-concurrent", type=int, default=2)
    source_add.add_argument("--no-gemini", action="store_true")
    source_add.add_argument("--disabled", action="store_true")
    source_add.add_argument("--metadata-json", default="{}")

    source_list = subparsers.add_parser("source-list", help="list registered sources")
    source_list.add_argument("--enabled-only", action="store_true")
    for name, enabled in (("source-enable", True), ("source-disable", False)):
        source_toggle = subparsers.add_parser(name, help=f"{name.replace('-', ' ')}")
        source_toggle.add_argument("name")
        source_toggle.set_defaults(source_enabled=enabled)

    discover = subparsers.add_parser("discover", help="discover normalized releases for a source")
    discover.add_argument("source")
    discover.add_argument("--no-fallback", action="store_true")

    ingest = subparsers.add_parser("ingest", help="discover, download, validate, and write a handoff")
    ingest.add_argument("source")
    ingest.add_argument("--output", default=None)
    ingest.add_argument("--release", dest="release_id")
    ingest.add_argument("--no-fallback", action="store_true")

    worker = subparsers.add_parser("worker", help="run the durable scraper worker")
    worker.add_argument("--once", action="store_true", help="process one job and exit")
    worker.add_argument("--poll-seconds", type=float, default=_env_float("SCRAPER_POLL_SECONDS", 5.0))
    worker.add_argument("--worker-id", default=os.environ.get("SCRAPER_WORKER_ID", f"scraper-{uuid.uuid4().hex[:8]}"))
    worker.add_argument("--lease-seconds", type=int, default=_env_int("SCRAPER_JOB_LEASE_SECONDS", 300))

    inspect = subparsers.add_parser("inspect-job", help="inspect one durable scraper job")
    inspect.add_argument("job_id")
    failures = subparsers.add_parser("list-failures", help="list failed scraper jobs")
    failures.add_argument("--limit", type=int, default=100)

    validate = subparsers.add_parser("validate", help="validate an already downloaded artifact")
    validate.add_argument("path")
    validate.add_argument("--url", default="https://validation.invalid/artifact.zip")
    validate.add_argument("--label", default="artifact")
    validate.add_argument("--reported-size", type=int)
    validate.add_argument("--checksum")

    subparsers.add_parser("adapter-list", help="list deterministic adapters")
    return parser


def _build_service(args: argparse.Namespace) -> ScraperService:
    allow_localhost = args.allow_localhost or _env_bool("SCRAPER_ALLOW_LOCALHOST", False)
    policy = URLPolicy(
        allow_localhost=allow_localhost,
        resolve_dns=not _env_bool("SCRAPER_SKIP_DNS_RESOLUTION", False),
        max_redirects=_env_int("SCRAPER_MAX_REDIRECTS", 8),
    )
    page_timeout = _env_float("SCRAPER_PAGE_TIMEOUT_SECONDS", 30.0)
    browser: BrowserExecutor
    if args.browser == "playwright":
        browser = PlaywrightBrowserExecutor(
            headless=not args.headful,
            executable_path=args.playwright_executable or None,
            profile_dir=args.profile_dir or None,
            policy=policy,
            navigation_timeout_ms=_env_int("SCRAPER_NAVIGATION_TIMEOUT_MS", 30_000),
        )
    else:
        browser = HttpBrowserExecutor(
            fetcher=HttpPageFetcher(
                policy=policy,
                max_page_bytes=_env_int("SCRAPER_MAX_PAGE_BYTES", 2 * 1024 * 1024),
                timeout_seconds=page_timeout,
            )
        )
    max_artifact_bytes = _env_int("SCRAPER_MAX_ARTIFACT_BYTES", 512 * 1024**3)
    downloader = HttpDownloader(
        policy=policy,
        scratch_budget=ScratchBudget(_env_int("SCRAPER_TEMP_MAX_BYTES", 2 * 512 * 1024**3)),
        timeout_seconds=_env_float("SCRAPER_DOWNLOAD_TIMEOUT_SECONDS", 120.0),
        max_artifact_bytes=max_artifact_bytes,
    )
    validator = ArtifactValidator(
        ValidationLimits(
            max_bytes=max_artifact_bytes,
            max_archive_entries=_env_int("SCRAPER_MAX_ARCHIVE_ENTRIES", 2_000_000),
            max_archive_bytes=_env_int("SCRAPER_MAX_ARCHIVE_BYTES", 4 * 1024**4),
            max_archive_file_bytes=_env_int("SCRAPER_MAX_ARCHIVE_FILE_BYTES", 512 * 1024**3),
        )
    )
    budget = PlannerBudget(
        max_pages_per_job=_env_int("SCRAPER_MAX_PAGES", 20),
        max_actions_per_page=_env_int("SCRAPER_MAX_ACTIONS_PER_PAGE", 8),
        max_total_actions=_env_int("SCRAPER_MAX_ACTIONS", 40),
        max_gemini_calls=_env_int("SCRAPER_MAX_GEMINI_CALLS", 6),
        max_navigation_depth=_env_int("SCRAPER_MAX_NAVIGATION_DEPTH", 6),
        max_runtime_seconds=_env_float("SCRAPER_JOB_TIMEOUT_SECONDS", 180.0),
    )
    return ScraperService(
        adapters=AdapterRegistry(),
        browser=browser,
        downloader=downloader,
        validator=validator,
        dedup=DedupIndex(args.dedup or None),
        url_policy=policy,
        planner_budget=budget,
    )


def _source(store: SQLiteJobStore, name: str) -> SourceDefinition:
    source = store.get_source(name)
    if source is None:
        raise JobStoreError(f"unknown scraper source {name}")
    return source


def _outcome_dict(outcome: ScrapeOutcome) -> dict[str, Any]:
    value: dict[str, Any] = {
        "status": outcome.status.value,
        "message": outcome.message,
        "releases": [release.to_dict() for release in outcome.releases],
        "visited_urls": list(outcome.visited_urls),
        "action_history": list(outcome.action_history),
        "gemini_calls": outcome.gemini_calls,
        "browser_actions": outcome.browser_actions,
    }
    if outcome.artifact is not None:
        value["artifact"] = outcome.artifact.to_dict()
    return value


def _print(value: Any) -> None:
    print(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False))


def _status_code(status: ScrapeStatus) -> int:
    return 0 if status == ScrapeStatus.SUCCESS else 1


def _dispatch(args: argparse.Namespace, store: SQLiteJobStore | None) -> int:
    if args.command == "adapter-list":
        _print({"adapters": list(AdapterRegistry().names())})
        return 0
    if store is None:
        raise RuntimeError("scraper state store is unavailable")
    if args.command == "source-add":
        try:
            metadata = json.loads(args.metadata_json)
        except json.JSONDecodeError as error:
            raise ValueError(f"--metadata-json must be an object: {error}") from error
        if not isinstance(metadata, dict):
            raise ValueError("--metadata-json must contain a JSON object")
        source = SourceDefinition(
            name=args.name,
            base_url=args.base_url,
            enabled=not args.disabled,
            adapter=args.adapter,
            check_interval_seconds=args.check_interval,
            platform_filters=tuple(args.platform or ("windows",)),
            language_filters=tuple(args.language),
            minimum_request_interval_seconds=args.min_request_interval,
            max_concurrent_requests=args.max_concurrent,
            gemini_fallback_allowed=not args.no_gemini,
            metadata=metadata,
        )
        store.add_source(source)
        _print(source.to_dict())
        return 0
    if args.command == "source-list":
        _print({"sources": [source.to_dict() for source in store.list_sources(args.enabled_only)]})
        return 0
    if args.command in {"source-enable", "source-disable"}:
        store.set_source_enabled(args.name, args.source_enabled)
        _print({"name": args.name, "enabled": args.source_enabled})
        return 0
    if args.command == "inspect-job":
        job = store.get_job(args.job_id)
        if job is None:
            raise JobStoreError(f"unknown scraper job {args.job_id}")
        _print(job.to_dict())
        return 0
    if args.command == "list-failures":
        _print({"jobs": [job.to_dict() for job in store.list_jobs(JobStatus.FAILED, args.limit)]})
        return 0

    service = _build_service(args)
    try:
        if args.command == "discover":
            source = _source(store, args.source)
            outcome = service.discover(source, allow_fallback=False if args.no_fallback else None)
            _print(_outcome_dict(outcome))
            return _status_code(outcome.status)
        if args.command == "ingest":
            source = _source(store, args.source)
            output = args.output or args.output_root
            outcome = service.ingest(
                source,
                output,
                target_release_id=args.release_id,
                allow_fallback=False if args.no_fallback else None,
            )
            _print(_outcome_dict(outcome))
            return _status_code(outcome.status)
        if args.command == "validate":
            candidate = DownloadCandidate(
                url=args.url,
                label=args.label,
                filename=Path(args.path).name,
                reported_size=args.reported_size,
                reported_checksum=args.checksum,
                confidence=1.0,
            )
            validation = ArtifactValidator().validate(args.path, candidate)
            _print(validation.to_dict())
            return 0 if validation.ok else 1
        if args.command == "worker":
            scheduler = IngestionScheduler(
                store,
                service,
                worker_id=args.worker_id,
                lease_seconds=args.lease_seconds,
                output_root=args.output_root,
                diagnostics=DiagnosticsWriter(
                    os.environ.get("SCRAPER_DIAGNOSTICS_DIR", "scraper-diagnostics"),
                    enabled=_env_bool("SCRAPER_DIAGNOSTICS_ENABLED", False),
                    max_bytes=_env_int("SCRAPER_DIAGNOSTICS_MAX_BYTES", 512 * 1024),
                ),
            )
            if args.once:
                result = scheduler.run_once()
                if result is None:
                    _print({"status": "IDLE"})
                    return 0
                _print({"job": result.job.to_dict(), "outcome": _outcome_dict(result.outcome)})
                return _status_code(result.outcome.status)
            scheduler.run_forever(poll_seconds=args.poll_seconds)
            return 0
    finally:
        service.browser.close()
    raise ValueError(f"unsupported command {args.command}")


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, str(args.log_level).upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )
    if args.command == "worker" and not _env_bool("SCRAPER_ENABLED", False):
        print("launcher-scraper: set SCRAPER_ENABLED=true before starting the worker", file=sys.stderr)
        return 2
    store: SQLiteJobStore | None = None
    try:
        if args.command != "adapter-list":
            store = SQLiteJobStore(args.store)
        return _dispatch(args, store)
    except (ValueError, OSError, JobStoreError) as error:
        print(f"launcher-scraper: {error}", file=sys.stderr)
        return 2
    finally:
        if store is not None:
            store.close()


if __name__ == "__main__":
    raise SystemExit(main())
