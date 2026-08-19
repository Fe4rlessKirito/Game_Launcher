from __future__ import annotations

from launcher_scraper.jobs import SQLiteJobStore
from launcher_scraper.models import JobStatus, ScrapeStatus, SourceDefinition
from launcher_scraper.scheduler import IngestionScheduler
from launcher_scraper.service import ScrapeOutcome


def test_sqlite_jobs_are_deduplicated_and_recovered(tmp_path) -> None:
    store = SQLiteJobStore(tmp_path / "state.db")
    source = SourceDefinition("fixture", "https://example.test/release")
    store.add_source(source)
    first = store.enqueue(source.name)
    duplicate = store.enqueue(source.name)
    assert first.id == duplicate.id

    claimed = store.claim("worker", lease_seconds=30)
    assert claimed is not None
    assert claimed.status == JobStatus.DISCOVERING
    claimed.lease_until = "2000-01-01T00:00:00+00:00"
    store.save(claimed)
    assert store.recover_expired() == 1
    assert store.get_job(first.id).status == JobStatus.RETRY
    store.close()


def test_scheduler_enqueues_due_sources_and_records_terminal_result(tmp_path) -> None:
    store = SQLiteJobStore(tmp_path / "state.db")
    source = SourceDefinition("fixture", "https://example.test/release", check_interval_seconds=3600)
    store.add_source(source)

    class _Service:
        def discover(self, source, *, interpreter=None):
            return ScrapeOutcome(ScrapeStatus.NOT_FOUND, message="no release")

        def acquire(self, *args, **kwargs):  # pragma: no cover - discovery is terminal here
            raise AssertionError

    scheduler = IngestionScheduler(store, _Service(), output_root=tmp_path / "artifacts")
    result = scheduler.run_once()

    assert result is not None
    assert result.outcome.status == ScrapeStatus.NOT_FOUND
    saved = store.get_job(result.job.id)
    assert saved is not None
    assert saved.status == JobStatus.FAILED
    assert saved.result_status == ScrapeStatus.NOT_FOUND
    assert store.due_sources() == []
    store.close()
