from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .models import SourceDefinition
from .security import safe_filename

if TYPE_CHECKING:
    from .service import ScrapeOutcome


class DiagnosticsWriter:
    """Write bounded, HTML-free failure diagnostics for operator review."""

    def __init__(self, root: str | Path, *, enabled: bool = True, max_bytes: int = 512 * 1024) -> None:
        if max_bytes < 1024:
            raise ValueError("diagnostics max_bytes must be at least 1024")
        self.root = Path(root)
        self.enabled = enabled
        self.max_bytes = max_bytes

    def write(self, job_id: str, source: SourceDefinition, outcome: ScrapeOutcome) -> str | None:
        if not self.enabled:
            return None
        value: dict[str, Any] = {
            "schema_version": 1,
            "job_id": job_id,
            "source": source.name,
            "domain": source.domain,
            "status": outcome.status.value,
            "message": outcome.message,
            "visited_urls": list(outcome.visited_urls),
            "action_history": list(outcome.action_history),
            "gemini_calls": outcome.gemini_calls,
            "browser_actions": outcome.browser_actions,
            "release_count": len(outcome.releases),
        }
        if outcome.planner and outcome.planner.final_page:
            value["final_page"] = outcome.planner.final_page.compact_dict(visible_text_limit=8_000)
        if outcome.artifact:
            value["artifact"] = outcome.artifact.to_dict()
        encoded = json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True).encode("utf-8")
        if len(encoded) > self.max_bytes:
            value = {
                "schema_version": 1,
                "job_id": job_id,
                "source": source.name,
                "status": outcome.status.value,
                "message": outcome.message,
                "visited_urls": list(outcome.visited_urls)[:32],
                "action_history": list(outcome.action_history)[:32],
                "gemini_calls": outcome.gemini_calls,
                "browser_actions": outcome.browser_actions,
                "truncated": True,
            }
            encoded = json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True).encode("utf-8")
        self.root.mkdir(parents=True, exist_ok=True)
        path = self.root / f"{safe_filename(job_id, 'job')}.json"
        temporary = path.with_suffix(path.suffix + ".tmp")
        temporary.write_bytes(encoded + b"\n")
        temporary.replace(path)
        return str(path)
