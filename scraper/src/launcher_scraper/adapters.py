from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from typing import Protocol
from urllib.parse import urlparse

from .models import DownloadCandidate, PageSnapshot, ReleaseCandidate, SourceDefinition

_VERSION_RE = re.compile(
    r"(?<![A-Za-z0-9])(?:v(?:ersion)?\s*)?([0-9]{1,4}(?:\.[0-9]{1,4}){1,4}(?:[-+][A-Za-z0-9][A-Za-z0-9._-]*)?)\b",
    re.I,
)
_SIZE_RE = re.compile(r"(?<![\w.])([0-9]+(?:\.[0-9]+)?)\s*(B|KB|MB|GB|TB)\b", re.I)
_CHECKSUM_RE = re.compile(r"\b(?:sha(?:-?256)?|blake3|md5)\s*[:=]\s*([0-9a-f]{32,128})\b", re.I)
_DOWNLOAD_RE = re.compile(r"\b(download|direct|installer|portable|mirror|archive|setup|release|get)\b", re.I)
_ARCHIVE_RE = re.compile(r"\.(?:zip|7z|rar|tar|tgz|tar\.gz|tar\.bz2|exe|msi|iso)(?:$|[?#])", re.I)
_AD_RE = re.compile(r"\b(ad|advert|sponsor|casino|vpn|notification|allow|subscribe|click here)\b", re.I)


class AdapterError(RuntimeError):
    pass


class LayoutChanged(AdapterError):
    pass


class SourceAdapter(Protocol):
    @property
    def name(self) -> str: ...

    def matches(self, source: SourceDefinition) -> bool: ...

    def discover(self, source: SourceDefinition, page: PageSnapshot) -> list[ReleaseCandidate]: ...

    def resolve_downloads(
        self, source: SourceDefinition, release: ReleaseCandidate
    ) -> tuple[DownloadCandidate, ...]: ...


def normalize_product_name(value: str) -> str:
    value = re.sub(r"\b(?:download|free|latest|official|full|version)\b", " ", value, flags=re.I)
    value = _VERSION_RE.sub(" ", value)
    value = re.sub(
        r"\b(?:windows?|win|linux|mac(?:os)?|darwin|x86|x64|arm64|aarch64|32[- ]?bit|64[- ]?bit)\b",
        " ",
        value,
        flags=re.I,
    )
    value = re.sub(r"[^A-Za-z0-9]+", " ", value)
    return re.sub(r"\s+", " ", value).strip().casefold()


def _extract_version(page: PageSnapshot) -> str:
    candidates = [page.metadata.get("version", ""), page.title, *page.headings, page.visible_text[:3000]]
    found: list[str] = []
    for value in candidates:
        found.extend(match.group(1) for match in _VERSION_RE.finditer(value))
    if not found:
        return "unknown"
    return max(found, key=lambda value: (len(value.split(".")), len(value), value))


def _size_from_text(value: str) -> int | None:
    match = _SIZE_RE.search(value)
    if not match:
        return None
    amount = float(match.group(1))
    multiplier = {"b": 1, "kb": 1024, "mb": 1024**2, "gb": 1024**3, "tb": 1024**4}[match.group(2).casefold()]
    return int(amount * multiplier)


def _checksum_from_text(value: str) -> str | None:
    match = _CHECKSUM_RE.search(value)
    return match.group(1).casefold() if match else None


def _candidate_architecture(candidate: DownloadCandidate) -> str:
    haystack = f"{candidate.label} {candidate.filename or ''} {candidate.url}".casefold()
    if any(token in haystack for token in ("arm64", "aarch64")):
        return "arm64"
    if any(token in haystack for token in ("x64", "x86_64", "amd64", "win64", "64-bit", "64 bit")):
        return "x64"
    if any(token in haystack for token in ("x86", "i386", "win32", "32-bit", "32 bit")):
        return "x86"
    return "unknown"


def _sidecar_base_name(href: str) -> str | None:
    filename = urlparse(href).path.rsplit("/", 1)[-1].casefold()
    for suffix in (".torrent", ".sha256", ".sha1", ".md5", ".sig", ".asc"):
        if filename.endswith(suffix):
            return filename[: -len(suffix)]
    return None


def _candidate_score(
    source: SourceDefinition, text: str, href: str, link_id: str, downloads: tuple[str, ...]
) -> tuple[float, list[str]]:
    haystack = f"{text} {href}".casefold()
    score = 0.05
    evidence: list[str] = []
    if link_id in downloads:
        score += 0.35
        evidence.append("semantic download indicator")
    if _ARCHIVE_RE.search(href):
        score += 0.35
        evidence.append("recognized artifact extension")
    if _DOWNLOAD_RE.search(text):
        score += 0.2
        evidence.append("download-like link text")
    if urlparse(href).hostname == source.domain:
        score += 0.05
        evidence.append("same source domain")
    if _AD_RE.search(haystack):
        score -= 0.45
        evidence.append("advertising or unrelated wording")
    if any(token in haystack for token in ("login", "register", "privacy", "terms", "telegram", "facebook")):
        score -= 0.25
        evidence.append("non-artifact navigation wording")
    return max(0.0, min(1.0, score)), evidence


@dataclass(frozen=True)
class GenericReleaseAdapter:
    """Deterministic adapter for conventional release pages.

    This adapter intentionally ranks candidates conservatively. Site-specific
    adapters can be registered without changing the browser, downloader, or
    validation layers.
    """

    name: str = "generic"
    minimum_download_confidence: float = 0.55

    def matches(self, source: SourceDefinition) -> bool:
        return source.adapter.casefold() in {"generic", "direct", self.name}

    def discover(self, source: SourceDefinition, page: PageSnapshot) -> list[ReleaseCandidate]:
        if not page.title and not page.headings and not page.visible_text:
            raise LayoutChanged("release page has no readable content")
        product = page.metadata.get("og:title") or page.metadata.get("twitter:title") or page.title
        if not product and page.headings:
            product = page.headings[0]
        product = re.sub(r"\s*[|–-]\s*(download|release|official).*$", "", product, flags=re.I).strip()
        if not product:
            raise LayoutChanged("release page has no product title")
        version = _extract_version(page)
        candidates = self._download_candidates(source, page)
        confidence = 0.45
        evidence = ["generic deterministic adapter"]
        if version != "unknown":
            confidence += 0.2
            evidence.append("version pattern")
        if candidates:
            confidence += 0.25
            evidence.append("ranked download candidate")
        else:
            evidence.append("no deterministic download candidate")
        release_id = hashlib.sha256(f"{source.name}|{page.url}|{version}".encode()).hexdigest()[:32]
        platform = self._platform(source, page)
        architecture = self._architecture(page)
        language = self._language(source, page)
        reported_size = _size_from_text(page.visible_text)
        reported_checksum = _checksum_from_text(page.visible_text)
        return [
            ReleaseCandidate(
                source=source.name,
                source_release_id=release_id,
                product_name=product[:300],
                normalized_product_name=normalize_product_name(product)[:300],
                version=version,
                release_date=page.metadata.get("datepublished") or page.metadata.get("article:published_time"),
                platform=platform,
                architecture=architecture,
                language=language,
                edition=page.metadata.get("edition"),
                source_page_url=page.url,
                download_candidates=tuple(candidates),
                reported_size=reported_size,
                reported_checksum=reported_checksum,
                confidence=max(0.0, min(1.0, confidence)),
                evidence=tuple(evidence),
                metadata={"adapter": self.name, "page_state_hash": page.state_hash},
            )
        ]

    def resolve_downloads(self, source: SourceDefinition, release: ReleaseCandidate) -> tuple[DownloadCandidate, ...]:
        preferred_architecture = release.architecture.casefold()

        def sort_key(candidate: DownloadCandidate) -> tuple[float, int, str]:
            architecture = _candidate_architecture(candidate)
            if preferred_architecture in {"x64", "x86", "arm64"}:
                architecture_rank = (
                    0 if architecture == preferred_architecture else 1 if architecture == "unknown" else 2
                )
            else:
                architecture_rank = {"x64": 0, "arm64": 1, "x86": 2, "unknown": 3}[architecture]
            return (-candidate.confidence, architecture_rank, candidate.url)

        return tuple(sorted(release.download_candidates, key=sort_key))

    def _download_candidates(self, source: SourceDefinition, page: PageSnapshot) -> list[DownloadCandidate]:
        sidecar_sizes: dict[str, int] = {}
        for link in page.links:
            base_name = _sidecar_base_name(link.href)
            size = _size_from_text(f"{link.text} {link.context}")
            if base_name and size is not None:
                sidecar_sizes[base_name] = size

        ranked: list[DownloadCandidate] = []
        for link in page.links:
            if _sidecar_base_name(link.href) is not None:
                continue
            score, evidence = _candidate_score(source, link.text, link.href, link.id, page.downloads_detected)
            if score < self.minimum_download_confidence:
                continue
            label = link.text or link.href.rsplit("/", 1)[-1]
            filename = link.href.rsplit("/", 1)[-1].split("?", 1)[0] or None
            ranked.append(
                DownloadCandidate(
                    url=link.href,
                    label=label[:300],
                    filename=filename,
                    reported_size=_size_from_text(f"{label} {link.context}")
                    or sidecar_sizes.get(filename.casefold() if filename else ""),
                    reported_checksum=_checksum_from_text(f"{label} {link.context}"),
                    confidence=score,
                    evidence=tuple(evidence),
                    browser_target_id=link.id,
                )
            )
        deduped: dict[str, DownloadCandidate] = {}
        for candidate in ranked:
            previous = deduped.get(candidate.url)
            if previous is None or candidate.confidence > previous.confidence:
                deduped[candidate.url] = candidate
        return sorted(deduped.values(), key=lambda item: (-item.confidence, item.url))[:20]

    @staticmethod
    def _platform(source: SourceDefinition, page: PageSnapshot) -> str:
        text = f"{page.title} {page.visible_text}".casefold()
        for platform in source.platform_filters:
            if platform.casefold() in text:
                return platform.casefold()
        return source.platform_filters[0].casefold() if source.platform_filters else "unknown"

    @staticmethod
    def _architecture(page: PageSnapshot) -> str:
        text = f"{page.title} {page.visible_text}".casefold()
        if any(token in text for token in ("arm64", "aarch64")):
            return "arm64"
        if any(token in text for token in ("x64", "x86_64", "64-bit", "64 bit")):
            return "x64"
        if any(token in text for token in ("x86", "32-bit", "32 bit")):
            return "x86"
        return "unknown"

    @staticmethod
    def _language(source: SourceDefinition, page: PageSnapshot) -> str | None:
        text = f"{page.title} {page.visible_text}".casefold()
        return next((language for language in source.language_filters if language.casefold() in text), None)


class AdapterRegistry:
    def __init__(self, adapters: list[SourceAdapter] | None = None) -> None:
        self._adapters: dict[str, SourceAdapter] = {}
        for adapter in adapters or [GenericReleaseAdapter()]:
            self.register(adapter)

    def register(self, adapter: SourceAdapter) -> None:
        self._adapters[adapter.name.casefold()] = adapter

    def get(self, source: SourceDefinition) -> SourceAdapter:
        adapter = self._adapters.get(source.adapter.casefold())
        if adapter is not None and adapter.matches(source):
            return adapter
        for candidate in self._adapters.values():
            if candidate.matches(source):
                return candidate
        raise AdapterError(f"no adapter registered for {source.adapter}")

    def names(self) -> tuple[str, ...]:
        return tuple(sorted(self._adapters))
