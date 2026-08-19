"""Bounded server-side release scraping for the Vaultnode ingestion pipeline."""

from .adapters import AdapterRegistry, GenericReleaseAdapter, SourceAdapter
from .diagnostics import DiagnosticsWriter
from .models import (
    ArtifactKind,
    ArtifactValidation,
    DownloadCandidate,
    JobStatus,
    PageAction,
    PageActionType,
    PageSnapshot,
    ReleaseCandidate,
    ScrapeJob,
    ScrapeStatus,
    SourceDefinition,
    ValidatedArtifact,
)
from .scheduler import IngestionScheduler, WorkerResult
from .service import ScraperService

__all__ = [
    "AdapterRegistry",
    "ArtifactKind",
    "ArtifactValidation",
    "DownloadCandidate",
    "DiagnosticsWriter",
    "GenericReleaseAdapter",
    "IngestionScheduler",
    "JobStatus",
    "PageAction",
    "PageActionType",
    "PageSnapshot",
    "ReleaseCandidate",
    "ScrapeJob",
    "ScrapeStatus",
    "ScraperService",
    "SourceAdapter",
    "SourceDefinition",
    "ValidatedArtifact",
    "WorkerResult",
]
