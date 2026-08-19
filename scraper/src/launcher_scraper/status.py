from __future__ import annotations

import json
import os
import re
from datetime import datetime
from pathlib import Path
from threading import Lock
from typing import Any

from .models import iso_now

_STATUS_ID_RE = re.compile(r"^[A-Za-z0-9_.-]+$")
WORK_STATUS_SCHEMA_VERSION = 1


class WorkStatusPublisher:
    """Publish advisory active-work records for the public operations status."""

    def __init__(self, directory: str | Path) -> None:
        self.directory = Path(directory)
        self._lock = Lock()

    def publish(
        self,
        status_id: str,
        *,
        kind: str,
        state: str,
        game: str | None = None,
        version: str | None = None,
        provider: str | None = None,
        detail: str = "",
        progress_percent: float | None = None,
    ) -> None:
        self._validate_id(status_id)
        now = iso_now()
        with self._lock:
            created_at = self._existing_created_at(status_id) or now
            payload: dict[str, Any] = {
                "schema_version": WORK_STATUS_SCHEMA_VERSION,
                "id": status_id,
                "kind": kind,
                "state": state,
                "game": game,
                "version": version,
                "provider": provider,
                "detail": detail[:300],
                "progress_percent": (
                    None if progress_percent is None else max(0.0, min(100.0, float(progress_percent)))
                ),
                "created_at": created_at,
                "updated_at": now,
            }
            self.directory.mkdir(parents=True, exist_ok=True)
            target = self.directory / f"{status_id}.json"
            temporary = self.directory / f".{status_id}.{os.getpid()}.tmp"
            temporary.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n", encoding="utf-8")
            temporary.replace(target)

    def clear(self, status_id: str) -> None:
        self._validate_id(status_id)
        with self._lock:
            (self.directory / f"{status_id}.json").unlink(missing_ok=True)

    def _existing_created_at(self, status_id: str) -> str | None:
        path = self.directory / f"{status_id}.json"
        try:
            value = json.loads(path.read_text(encoding="utf-8")).get("created_at")
        except (FileNotFoundError, OSError, json.JSONDecodeError, AttributeError):
            return None
        if not isinstance(value, str):
            return None
        try:
            datetime.fromisoformat(value)
        except ValueError:
            return None
        return value

    @staticmethod
    def _validate_id(status_id: str) -> None:
        if not status_id or _STATUS_ID_RE.fullmatch(status_id) is None:
            raise ValueError("work status id contains unsupported characters")
