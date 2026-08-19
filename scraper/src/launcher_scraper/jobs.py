from __future__ import annotations

import json
import sqlite3
import time
import uuid
from datetime import UTC, datetime
from pathlib import Path
from threading import Lock

from .models import JobStatus, ScrapeJob, SourceDefinition, iso_now


class JobStoreError(RuntimeError):
    pass


class SQLiteJobStore:
    """Durable standalone scraper state store.

    The store is intentionally independent from the launcher client. A later
    deployment can replace it with a PostgreSQL implementation without
    changing scraper orchestration because all callers use this small store
    contract. SQLite WAL mode makes a single server-side scraper process and
    its restart recovery safe without adding a new service dependency.
    """

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = Lock()
        self._connection = sqlite3.connect(self.path, check_same_thread=False)
        self._connection.row_factory = sqlite3.Row
        self._connection.execute("PRAGMA journal_mode=WAL")
        self._connection.execute("PRAGMA busy_timeout=5000")
        self._initialize()

    def _initialize(self) -> None:
        with self._connection:
            self._connection.executescript(
                """
                CREATE TABLE IF NOT EXISTS scraper_sources (
                    name TEXT PRIMARY KEY,
                    config_json TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    next_check_at REAL NOT NULL DEFAULT 0,
                    last_checked_at REAL,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                );
                CREATE TABLE IF NOT EXISTS scraper_jobs (
                    id TEXT PRIMARY KEY,
                    source_name TEXT NOT NULL,
                    target_release_id TEXT,
                    status TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    max_attempts INTEGER NOT NULL DEFAULT 5,
                    lease_until REAL,
                    payload_json TEXT NOT NULL,
                    last_error TEXT,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS scraper_jobs_claim_idx
                    ON scraper_jobs(status, lease_until, updated_at);
                CREATE INDEX IF NOT EXISTS scraper_jobs_source_idx
                    ON scraper_jobs(source_name, target_release_id, status);
                """
            )

    def close(self) -> None:
        with self._lock:
            self._connection.close()

    def add_source(self, source: SourceDefinition) -> None:
        now = time.time()
        payload = json.dumps(source.to_dict(), sort_keys=True)
        with self._lock, self._connection:
            self._connection.execute(
                """
                INSERT INTO scraper_sources(name, config_json, enabled, next_check_at, created_at, updated_at)
                VALUES(?, ?, ?, 0, ?, ?)
                ON CONFLICT(name) DO UPDATE SET config_json=excluded.config_json,
                    enabled=excluded.enabled, updated_at=excluded.updated_at
                """,
                (source.name, payload, int(source.enabled), now, now),
            )

    def get_source(self, name: str) -> SourceDefinition | None:
        row = self._connection.execute("SELECT config_json FROM scraper_sources WHERE name=?", (name,)).fetchone()
        if row is None:
            return None
        return SourceDefinition(**json.loads(row["config_json"]))

    def list_sources(self, enabled_only: bool = False) -> list[SourceDefinition]:
        query = "SELECT config_json FROM scraper_sources"
        if enabled_only:
            query += " WHERE enabled=1"
        query += " ORDER BY name"
        return [SourceDefinition(**json.loads(row["config_json"])) for row in self._connection.execute(query)]

    def set_source_enabled(self, name: str, enabled: bool) -> None:
        now = time.time()
        with self._lock, self._connection:
            cursor = self._connection.execute(
                "UPDATE scraper_sources SET enabled=?, updated_at=? WHERE name=?",
                (int(enabled), now, name),
            )
            if cursor.rowcount == 0:
                raise JobStoreError(f"unknown scraper source {name}")

    def due_sources(self, now: float | None = None) -> list[SourceDefinition]:
        now = time.time() if now is None else now
        rows = self._connection.execute(
            "SELECT config_json FROM scraper_sources WHERE enabled=1 AND next_check_at <= ? ORDER BY name", (now,)
        )
        return [SourceDefinition(**json.loads(row["config_json"])) for row in rows]

    def mark_source_checked(self, name: str, next_check_at: float) -> None:
        now = time.time()
        with self._lock, self._connection:
            self._connection.execute(
                "UPDATE scraper_sources SET next_check_at=?, last_checked_at=?, updated_at=? WHERE name=?",
                (next_check_at, now, now, name),
            )

    def enqueue(self, source_name: str, target_release_id: str | None = None, max_attempts: int = 5) -> ScrapeJob:
        active = (
            JobStatus.QUEUED.value,
            JobStatus.DISCOVERING.value,
            JobStatus.ACQUIRING.value,
            JobStatus.VALIDATING.value,
            JobStatus.RETRY.value,
        )
        placeholders = ",".join("?" for _ in active)
        with self._lock, self._connection:
            row = self._connection.execute(
                f"""
                SELECT payload_json FROM scraper_jobs
                WHERE source_name=? AND target_release_id IS ?
                  AND status IN ({placeholders})
                ORDER BY created_at LIMIT 1
                """,
                (source_name, target_release_id, *active),
            ).fetchone()
            if row is not None:
                return ScrapeJob.from_dict(json.loads(row["payload_json"]))
            job = ScrapeJob(
                str(uuid.uuid4()), source_name, target_release_id=target_release_id, max_attempts=max_attempts
            )
            self._insert_job(job)
            return job

    def claim(self, worker_id: str, lease_seconds: int = 180) -> ScrapeJob | None:
        now = time.time()
        lease = now + max(1, lease_seconds)
        active = (JobStatus.QUEUED.value, JobStatus.RETRY.value)
        with self._lock, self._connection:
            row = self._connection.execute(
                """
                SELECT * FROM scraper_jobs
                WHERE status IN (?, ?)
                  AND (lease_until IS NULL OR lease_until < ?)
                  AND attempts < max_attempts
                ORDER BY updated_at, created_at LIMIT 1
                """,
                (*active, now),
            ).fetchone()
            if row is None:
                return None
            job = self._row_to_job(row)
            job.attempts += 1
            job.status = JobStatus.DISCOVERING
            job.stage = "DISCOVERY"
            job.lease_until = _iso_from_epoch(lease)
            job.updated_at = iso_now()
            self._update_job(job, lease)
            return job

    def save(self, job: ScrapeJob, lease_seconds: int | None = None) -> None:
        lease = None
        if lease_seconds is not None:
            lease = time.time() + max(1, lease_seconds)
            job.lease_until = _iso_from_epoch(lease)
        with self._lock, self._connection:
            self._update_job(job, lease)

    def heartbeat(self, job: ScrapeJob, lease_seconds: int = 180) -> None:
        self.save(job, lease_seconds)

    def complete(self, job: ScrapeJob) -> None:
        job.status = JobStatus.DONE
        job.stage = "DONE"
        job.lease_until = None
        job.updated_at = iso_now()
        self.save(job)

    def fail(self, job: ScrapeJob, error: str, retry: bool) -> None:
        job.status = JobStatus.RETRY if retry and job.attempts < job.max_attempts else JobStatus.FAILED
        job.stage = job.status.value
        job.last_error = error[:4000]
        job.lease_until = None
        job.updated_at = iso_now()
        self.save(job)

    def cancel(self, job_id: str, reason: str = "cancelled") -> None:
        job = self.get_job(job_id)
        if job is None:
            raise JobStoreError(f"unknown scraper job {job_id}")
        job.status = JobStatus.CANCELLED
        job.stage = "CANCELLED"
        job.last_error = reason[:4000]
        job.lease_until = None
        self.save(job)

    def recover_expired(self, now: float | None = None) -> int:
        now = time.time() if now is None else now
        with self._lock, self._connection:
            rows = self._connection.execute(
                """
                SELECT * FROM scraper_jobs
                WHERE lease_until IS NOT NULL AND lease_until < ?
                  AND status NOT IN (?, ?, ?)
                """,
                (now, JobStatus.DONE.value, JobStatus.FAILED.value, JobStatus.CANCELLED.value),
            ).fetchall()
            for row in rows:
                job = self._row_to_job(row)
                job.status = JobStatus.RETRY if job.attempts < job.max_attempts else JobStatus.FAILED
                job.stage = job.status.value
                job.lease_until = None
                job.last_error = "lease expired; job recovered after scraper restart"
                self._update_job(job, None)
            return len(rows)

    def get_job(self, job_id: str) -> ScrapeJob | None:
        row = self._connection.execute("SELECT * FROM scraper_jobs WHERE id=?", (job_id,)).fetchone()
        return self._row_to_job(row) if row is not None else None

    def list_jobs(self, status: JobStatus | None = None, limit: int = 100) -> list[ScrapeJob]:
        if status is None:
            rows = self._connection.execute(
                "SELECT * FROM scraper_jobs ORDER BY updated_at DESC LIMIT ?", (min(500, max(1, limit)),)
            )
        else:
            rows = self._connection.execute(
                "SELECT * FROM scraper_jobs WHERE status=? ORDER BY updated_at DESC LIMIT ?",
                (status.value, min(500, max(1, limit))),
            )
        return [self._row_to_job(row) for row in rows]

    def _insert_job(self, job: ScrapeJob) -> None:
        now = time.time()
        self._connection.execute(
            """
            INSERT INTO scraper_jobs(
                id, source_name, target_release_id, status, stage, attempts,
                max_attempts, payload_json, created_at, updated_at
            ) VALUES(?,?,?,?,?,?,?,?,?,?)
            """,
            (
                job.id,
                job.source_name,
                job.target_release_id,
                job.status.value,
                job.stage,
                job.attempts,
                job.max_attempts,
                json.dumps(job.to_dict(), sort_keys=True),
                now,
                now,
            ),
        )

    def _update_job(self, job: ScrapeJob, lease_epoch: float | None) -> None:
        self._connection.execute(
            """
            UPDATE scraper_jobs SET target_release_id=?, status=?, stage=?,
                attempts=?, max_attempts=?, lease_until=?, payload_json=?,
                last_error=?, updated_at=? WHERE id=?
            """,
            (
                job.target_release_id,
                job.status.value,
                job.stage,
                job.attempts,
                job.max_attempts,
                lease_epoch if lease_epoch is not None else _epoch_from_iso(job.lease_until),
                json.dumps(job.to_dict(), sort_keys=True),
                job.last_error,
                time.time(),
                job.id,
            ),
        )

    @staticmethod
    def _row_to_job(row: sqlite3.Row) -> ScrapeJob:
        return ScrapeJob.from_dict(json.loads(row["payload_json"]))


def _iso_from_epoch(value: float) -> str:
    return datetime.fromtimestamp(value, UTC).isoformat()


def _epoch_from_iso(value: str | None) -> float | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value).timestamp()
    except ValueError:
        return None
