from __future__ import annotations

import json
from datetime import datetime

import pytest

from launcher_scraper.status import WorkStatusPublisher


def test_status_publisher_writes_and_clears_active_record(tmp_path) -> None:
    publisher = WorkStatusPublisher(tmp_path)

    publisher.publish(
        "job-1",
        kind="SCRAPER",
        state="DOWNLOADING",
        game="OpenTTD",
        version="15.3",
        source="openttd-official",
        detail="Downloading release",
        progress_percent=42,
        bytes_completed=100 * 1024 * 1024,
        bytes_total=1024 * 1024 * 1024,
        rate_bytes_per_second=8 * 1024 * 1024,
    )
    record = json.loads((tmp_path / "job-1.json").read_text(encoding="utf-8"))
    assert record["game"] == "OpenTTD"
    assert record["source"] == "openttd-official"
    assert record["progress_percent"] == 42.0
    assert record["bytes_completed"] == 100 * 1024 * 1024
    assert record["bytes_total"] == 1024 * 1024 * 1024
    assert record["rate_bytes_per_second"] == 8 * 1024 * 1024
    datetime.fromisoformat(record["created_at"])
    created_at = record["created_at"]

    publisher.publish("job-1", kind="SCRAPER", state="VALIDATING", game="OpenTTD", detail="Validating")
    updated = json.loads((tmp_path / "job-1.json").read_text(encoding="utf-8"))
    assert updated["created_at"] == created_at
    assert updated["state"] == "VALIDATING"

    publisher.clear("job-1")
    assert not (tmp_path / "job-1.json").exists()


def test_status_publisher_rejects_unsafe_ids(tmp_path) -> None:
    publisher = WorkStatusPublisher(tmp_path)

    with pytest.raises(ValueError):
        publisher.publish("../leak", kind="SCRAPER", state="QUEUED")
