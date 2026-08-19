from __future__ import annotations

import re
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from typing import Any
from urllib.parse import urlparse


def utc_now() -> datetime:
    return datetime.now(UTC)


def iso_now() -> str:
    return utc_now().isoformat()


def _clean_text(value: str, limit: int = 4000) -> str:
    return re.sub(r"\s+", " ", value or "").strip()[:limit]


class ScrapeStatus(StrEnum):
    SUCCESS = "SUCCESS"
    NOT_FOUND = "NOT_FOUND"
    LAYOUT_CHANGED = "LAYOUT_CHANGED"
    CHALLENGE_REQUIRED = "CHALLENGE_REQUIRED"
    RATE_LIMITED = "RATE_LIMITED"
    VALIDATION_FAILED = "VALIDATION_FAILED"
    TEMPORARY_FAILURE = "TEMPORARY_FAILURE"
    PERMANENT_FAILURE = "PERMANENT_FAILURE"
    MANUAL_REVIEW = "MANUAL_REVIEW"


class JobStatus(StrEnum):
    QUEUED = "QUEUED"
    DISCOVERING = "DISCOVERING"
    ACQUIRING = "ACQUIRING"
    VALIDATING = "VALIDATING"
    READY = "READY"
    RETRY = "RETRY"
    FAILED = "FAILED"
    CANCELLED = "CANCELLED"
    DONE = "DONE"


class PageActionType(StrEnum):
    FOLLOW_LINK = "FOLLOW_LINK"
    CLICK = "CLICK"
    SCROLL = "SCROLL"
    WAIT = "WAIT"
    GO_BACK = "GO_BACK"
    EXTRACT_RELEASE = "EXTRACT_RELEASE"
    REQUEST_MORE_CONTEXT = "REQUEST_MORE_CONTEXT"
    ABORT = "ABORT"


class ArtifactKind(StrEnum):
    ZIP = "zip"
    SEVEN_ZIP = "7z"
    RAR = "rar"
    TAR = "tar"
    TAR_GZIP = "tar.gz"
    TAR_BZIP2 = "tar.bz2"
    WINDOWS_EXECUTABLE = "exe"
    WINDOWS_INSTALLER = "msi"
    DISK_IMAGE = "disk-image"
    BINARY = "binary"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class SourceDefinition:
    name: str
    base_url: str
    enabled: bool = True
    adapter: str = "generic"
    check_interval_seconds: int = 3600
    platform_filters: tuple[str, ...] = ("windows",)
    language_filters: tuple[str, ...] = ()
    minimum_request_interval_seconds: float = 1.0
    max_concurrent_requests: int = 2
    gemini_fallback_allowed: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        parsed = urlparse(self.base_url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise ValueError("source base_url must be an absolute HTTP(S) URL")
        if not self.name.strip():
            raise ValueError("source name is required")
        if self.check_interval_seconds < 1:
            raise ValueError("check_interval_seconds must be positive")
        if self.minimum_request_interval_seconds < 0:
            raise ValueError("minimum_request_interval_seconds cannot be negative")
        if not 1 <= self.max_concurrent_requests <= 32:
            raise ValueError("max_concurrent_requests must be between 1 and 32")

    @property
    def domain(self) -> str:
        return (urlparse(self.base_url).hostname or "").casefold()

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True)
class SemanticLink:
    id: str
    text: str
    href: str
    context: str = ""
    rel: str = ""
    download: str = ""


@dataclass(frozen=True)
class SemanticButton:
    id: str
    text: str
    context: str = ""


@dataclass(frozen=True)
class SemanticForm:
    id: str
    action: str
    method: str
    fields: tuple[str, ...] = ()


@dataclass(frozen=True)
class ElementTarget:
    id: str
    kind: str
    ordinal: int


@dataclass(frozen=True)
class PageSnapshot:
    url: str
    title: str
    headings: tuple[str, ...] = ()
    visible_text: str = ""
    links: tuple[SemanticLink, ...] = ()
    buttons: tuple[SemanticButton, ...] = ()
    forms: tuple[SemanticForm, ...] = ()
    downloads_detected: tuple[str, ...] = ()
    breadcrumbs: tuple[str, ...] = ()
    pagination: tuple[str, ...] = ()
    metadata: dict[str, str] = field(default_factory=dict)
    targets: tuple[ElementTarget, ...] = ()
    state_hash: str = ""

    def target_ids(self) -> set[str]:
        return {target.id for target in self.targets}

    def target(self, target_id: str) -> ElementTarget | None:
        return next((target for target in self.targets if target.id == target_id), None)

    def compact_dict(self, visible_text_limit: int = 8000) -> dict[str, Any]:
        return {
            "url": self.url,
            "title": self.title,
            "headings": list(self.headings),
            "visible_text": self.visible_text[:visible_text_limit],
            "links": [asdict(link) for link in self.links],
            "buttons": [asdict(button) for button in self.buttons],
            "forms": [asdict(form) for form in self.forms],
            "downloads_detected": list(self.downloads_detected),
            "breadcrumbs": list(self.breadcrumbs),
            "pagination": list(self.pagination),
            "metadata": dict(self.metadata),
            "state_hash": self.state_hash,
        }


@dataclass(frozen=True)
class PageAction:
    action: PageActionType
    target_id: str | None = None
    reason: str = ""
    confidence: float = 0.0
    wait_seconds: float = 0.0
    scroll_delta: int = 0

    def __post_init__(self) -> None:
        if not 0.0 <= self.confidence <= 1.0:
            raise ValueError("action confidence must be between 0 and 1")
        if self.wait_seconds < 0 or self.wait_seconds > 30:
            raise ValueError("wait_seconds must be between 0 and 30")
        if self.action in {PageActionType.FOLLOW_LINK, PageActionType.CLICK} and not self.target_id:
            raise ValueError(f"{self.action} requires target_id")

    def to_dict(self) -> dict[str, Any]:
        return {
            "action": self.action.value,
            "target_id": self.target_id,
            "reason": self.reason,
            "confidence": self.confidence,
            "wait_seconds": self.wait_seconds,
            "scroll_delta": self.scroll_delta,
        }


@dataclass(frozen=True)
class DownloadCandidate:
    url: str
    label: str
    filename: str | None = None
    reported_size: int | None = None
    reported_checksum: str | None = None
    content_type: str | None = None
    confidence: float = 0.0
    evidence: tuple[str, ...] = ()
    requires_browser: bool = False
    browser_target_id: str | None = None

    def __post_init__(self) -> None:
        parsed = urlparse(self.url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise ValueError("download candidate URL must be HTTP(S)")
        if not 0.0 <= self.confidence <= 1.0:
            raise ValueError("candidate confidence must be between 0 and 1")


@dataclass(frozen=True)
class ReleaseCandidate:
    source: str
    source_release_id: str
    product_name: str
    normalized_product_name: str
    version: str
    release_date: str | None
    platform: str
    architecture: str
    language: str | None
    edition: str | None
    source_page_url: str
    download_candidates: tuple[DownloadCandidate, ...]
    reported_size: int | None = None
    reported_checksum: str | None = None
    discovered_at: str = field(default_factory=iso_now)
    confidence: float = 0.0
    evidence: tuple[str, ...] = ()
    metadata: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.source_release_id.strip():
            raise ValueError("source_release_id is required")
        if not self.product_name.strip() or not self.normalized_product_name.strip():
            raise ValueError("product_name is required")
        if not 0.0 <= self.confidence <= 1.0:
            raise ValueError("release confidence must be between 0 and 1")

    @property
    def best_download(self) -> DownloadCandidate | None:
        return max(self.download_candidates, key=lambda candidate: candidate.confidence, default=None)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True)
class ArtifactValidation:
    ok: bool
    kind: ArtifactKind
    errors: tuple[str, ...] = ()
    warnings: tuple[str, ...] = ()
    actual_size: int = 0
    blake3: str = ""
    sha256: str = ""
    archive_entries: int = 0
    archive_bytes: int = 0

    def to_dict(self) -> dict[str, Any]:
        return asdict(self) | {"kind": self.kind.value}


@dataclass(frozen=True)
class ValidatedArtifact:
    path: str
    filename: str
    source: str
    source_release_id: str
    source_page_url: str
    download_url: str
    validation: ArtifactValidation
    release: ReleaseCandidate
    normalized_metadata: dict[str, Any] = field(default_factory=dict)
    handoff_path: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "path": self.path,
            "filename": self.filename,
            "source": self.source,
            "source_release_id": self.source_release_id,
            "source_page_url": self.source_page_url,
            "download_url": self.download_url,
            "validation": self.validation.to_dict(),
            "release": self.release.to_dict(),
            "normalized_metadata": self.normalized_metadata,
            "handoff_path": self.handoff_path,
        }


@dataclass
class ScrapeJob:
    id: str
    source_name: str
    target_release_id: str | None = None
    status: JobStatus = JobStatus.QUEUED
    stage: str = "QUEUED"
    attempts: int = 0
    max_attempts: int = 5
    created_at: str = field(default_factory=iso_now)
    updated_at: str = field(default_factory=iso_now)
    lease_until: str | None = None
    visited_urls: list[str] = field(default_factory=list)
    action_history: list[dict[str, Any]] = field(default_factory=list)
    resolved_artifact: dict[str, Any] | None = None
    last_error: str | None = None
    result_status: ScrapeStatus | None = None
    gemini_calls: int = 0
    browser_actions: int = 0

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        value["status"] = self.status.value
        value["result_status"] = self.result_status.value if self.result_status else None
        return value

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> ScrapeJob:
        data = dict(value)
        data["status"] = JobStatus(data.get("status", JobStatus.QUEUED))
        result_status = data.get("result_status")
        data["result_status"] = ScrapeStatus(result_status) if result_status else None
        return cls(**data)


@dataclass(frozen=True)
class PlannerBudget:
    max_pages_per_job: int = 20
    max_actions_per_page: int = 8
    max_total_actions: int = 40
    max_gemini_calls: int = 6
    max_navigation_depth: int = 6
    max_runtime_seconds: float = 180.0
    max_download_attempts: int = 3

    def __post_init__(self) -> None:
        for name in (
            "max_pages_per_job",
            "max_actions_per_page",
            "max_total_actions",
            "max_gemini_calls",
            "max_navigation_depth",
            "max_download_attempts",
        ):
            if getattr(self, name) < 1:
                raise ValueError(f"{name} must be positive")
        if self.max_runtime_seconds <= 0:
            raise ValueError("max_runtime_seconds must be positive")
