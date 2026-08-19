from __future__ import annotations

import json
import stat
import tarfile
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

from .downloader import DownloadResult, _checksum_matches, _hash_file
from .models import ArtifactKind, ArtifactValidation, DownloadCandidate
from .security import safe_filename


class ArtifactValidationError(RuntimeError):
    pass


@dataclass(frozen=True)
class ValidationLimits:
    max_bytes: int = 512 * 1024**3
    max_archive_entries: int = 2_000_000
    max_archive_bytes: int = 4 * 1024**4
    max_archive_file_bytes: int = 512 * 1024**3
    max_header_bytes: int = 64 * 1024


class ArtifactValidator:
    def __init__(self, limits: ValidationLimits | None = None) -> None:
        self.limits = limits or ValidationLimits()

    def validate(
        self,
        path: str | Path,
        candidate: DownloadCandidate,
        *,
        download: DownloadResult | None = None,
    ) -> ArtifactValidation:
        artifact = Path(path)
        errors: list[str] = []
        warnings: list[str] = []
        if not artifact.is_file() or artifact.is_symlink():
            return ArtifactValidation(False, ArtifactKind.UNKNOWN, ("artifact is not a regular file",))
        actual_size = artifact.stat().st_size
        if actual_size <= 0:
            errors.append("artifact is empty")
        if actual_size > self.limits.max_bytes:
            errors.append(f"artifact exceeds {self.limits.max_bytes} bytes")
        if candidate.reported_size is not None and actual_size != candidate.reported_size:
            errors.append(f"reported size mismatch: expected {candidate.reported_size}, got {actual_size}")

        blake3_value, sha256_value = _hash_file(artifact)
        expected_checksum = candidate.reported_checksum
        if expected_checksum and not _checksum_matches(expected_checksum, blake3_value, sha256_value):
            errors.append("reported checksum does not match artifact")

        headers = download.headers if download else {}
        content_type = headers.get("content-type", "").split(";", 1)[0].casefold()
        if content_type == "text/html" or content_type == "application/xhtml+xml":
            errors.append("server returned HTML instead of an artifact")
        with artifact.open("rb") as stream:
            header = stream.read(self.limits.max_header_bytes)
        kind = _detect_kind(header, artifact.name)
        if _looks_like_html(header):
            errors.append("artifact content looks like HTML")
        extension_kind = _extension_kind(artifact.name)
        if extension_kind is not None and kind != extension_kind:
            errors.append(f"filename extension suggests {extension_kind.value}, magic bytes indicate {kind.value}")
        if kind == ArtifactKind.UNKNOWN and content_type.startswith("text/"):
            errors.append(f"unknown binary format with text content type {content_type}")
        if kind == ArtifactKind.UNKNOWN:
            warnings.append(
                "artifact format is unknown; the downstream normalizer must perform final format validation"
            )
        if download and (download.status < 200 or download.status >= 300):
            errors.append(f"download status was not successful: {download.status}")

        archive_entries = 0
        archive_bytes = 0
        if kind == ArtifactKind.ZIP:
            archive_entries, archive_bytes, archive_errors = self._inspect_zip(artifact)
            errors.extend(archive_errors)
        elif kind in {ArtifactKind.TAR, ArtifactKind.TAR_GZIP, ArtifactKind.TAR_BZIP2}:
            archive_entries, archive_bytes, archive_errors = self._inspect_tar(artifact)
            errors.extend(archive_errors)
        elif kind in {ArtifactKind.RAR, ArtifactKind.SEVEN_ZIP}:
            warnings.append(
                "archive framing is recognized; the existing Rust normalizer performs bounded member validation"
            )

        return ArtifactValidation(
            ok=not errors,
            kind=kind,
            errors=tuple(dict.fromkeys(errors)),
            warnings=tuple(dict.fromkeys(warnings)),
            actual_size=actual_size,
            blake3=blake3_value,
            sha256=sha256_value,
            archive_entries=archive_entries,
            archive_bytes=archive_bytes,
        )

    def _inspect_zip(self, path: Path) -> tuple[int, int, list[str]]:
        errors: list[str] = []
        names: set[str] = set()
        total = 0
        entries = 0
        try:
            with zipfile.ZipFile(path) as archive:
                for info in archive.infolist():
                    entries += 1
                    if entries > self.limits.max_archive_entries:
                        errors.append("archive contains too many entries")
                        break
                    normalized = _validate_archive_name(info.filename)
                    if normalized is None:
                        errors.append(f"unsafe archive path: {info.filename!r}")
                    elif normalized.casefold() in names:
                        errors.append(f"duplicate archive path: {info.filename!r}")
                    else:
                        names.add(normalized.casefold())
                    if info.file_size > self.limits.max_archive_file_bytes:
                        errors.append(f"archive member exceeds file limit: {info.filename!r}")
                    total += max(0, info.file_size)
                    if total > self.limits.max_archive_bytes:
                        errors.append("archive expanded size exceeds limit")
                        break
                    mode = (info.external_attr >> 16) & 0xFFFF
                    if stat.S_ISLNK(mode):
                        errors.append(f"archive contains a symbolic link: {info.filename!r}")
                if not errors:
                    bad = archive.testzip()
                    if bad:
                        errors.append(f"archive CRC validation failed: {bad}")
        except (OSError, zipfile.BadZipFile, RuntimeError) as error:
            errors.append(f"invalid ZIP archive: {error}")
        return entries, total, errors

    def _inspect_tar(self, path: Path) -> tuple[int, int, list[str]]:
        errors: list[str] = []
        names: set[str] = set()
        total = 0
        entries = 0
        try:
            with tarfile.open(path, mode="r:*") as archive:
                for member in archive:
                    entries += 1
                    if entries > self.limits.max_archive_entries:
                        errors.append("archive contains too many entries")
                        break
                    normalized = _validate_archive_name(member.name)
                    if normalized is None:
                        errors.append(f"unsafe archive path: {member.name!r}")
                    elif normalized.casefold() in names:
                        errors.append(f"duplicate archive path: {member.name!r}")
                    else:
                        names.add(normalized.casefold())
                    if member.issym() or member.islnk() or member.isdev() or member.isfifo():
                        errors.append(f"archive contains unsupported link/device entry: {member.name!r}")
                    if member.size > self.limits.max_archive_file_bytes:
                        errors.append(f"archive member exceeds file limit: {member.name!r}")
                    total += max(0, member.size)
                    if total > self.limits.max_archive_bytes:
                        errors.append("archive expanded size exceeds limit")
                        break
        except (OSError, tarfile.TarError) as error:
            errors.append(f"invalid TAR archive: {error}")
        return entries, total, errors


class DedupIndex:
    """Small durable hash index used to avoid reprocessing identical artifacts."""

    def __init__(self, path: str | Path | None = None) -> None:
        self.path = Path(path) if path else None
        self._values: dict[str, dict[str, Any]] = {}
        if self.path and self.path.is_file():
            try:
                value = json.loads(self.path.read_text(encoding="utf-8"))
                if isinstance(value, dict):
                    self._values = {str(key): item for key, item in value.items() if isinstance(item, dict)}
            except (OSError, json.JSONDecodeError):
                self._values = {}

    def lookup(self, blake3_value: str) -> dict[str, Any] | None:
        return self._values.get(blake3_value)

    def record(self, blake3_value: str, metadata: dict[str, Any]) -> None:
        self._values[blake3_value] = dict(metadata)
        if self.path:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            temp = self.path.with_suffix(self.path.suffix + ".tmp")
            temp.write_text(json.dumps(self._values, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            temp.replace(self.path)


def _looks_like_html(header: bytes) -> bool:
    sample = header[:16_384].lstrip().lower()
    return sample.startswith((b"<!doctype html", b"<html", b"<head", b"<body")) or b"<html" in sample[:1024]


def _detect_kind(header: bytes, filename: str) -> ArtifactKind:
    lower = filename.casefold()
    if header.startswith((b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08")):
        return ArtifactKind.ZIP
    if header.startswith(b"7z\xbc\xaf'\x1c"):
        return ArtifactKind.SEVEN_ZIP
    if header.startswith(b"Rar!\x1a\x07\x00") or header.startswith(b"Rar!\x1a\x07\x01\x00"):
        return ArtifactKind.RAR
    if header.startswith(b"\x1f\x8b"):
        return ArtifactKind.TAR_GZIP if lower.endswith((".tar.gz", ".tgz")) else ArtifactKind.BINARY
    if header.startswith(b"BZh"):
        return ArtifactKind.TAR_BZIP2 if lower.endswith((".tar.bz2", ".tbz2")) else ArtifactKind.BINARY
    if len(header) >= 265 and header[257:262] == b"ustar":
        return ArtifactKind.TAR
    if header.startswith(b"MZ"):
        return ArtifactKind.WINDOWS_EXECUTABLE
    if header.startswith(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"):
        return ArtifactKind.WINDOWS_INSTALLER if lower.endswith(".msi") else ArtifactKind.BINARY
    if len(header) >= 32_773 and header[32_769:32_774] == b"CD001":
        return ArtifactKind.DISK_IMAGE
    if header:
        return ArtifactKind.BINARY if not _looks_like_html(header) else ArtifactKind.UNKNOWN
    return ArtifactKind.UNKNOWN


def _extension_kind(filename: str) -> ArtifactKind | None:
    lower = safe_filename(filename).casefold()
    if lower.endswith(".zip"):
        return ArtifactKind.ZIP
    if lower.endswith(".7z"):
        return ArtifactKind.SEVEN_ZIP
    if lower.endswith(".rar"):
        return ArtifactKind.RAR
    if lower.endswith((".tar.gz", ".tgz")):
        return ArtifactKind.TAR_GZIP
    if lower.endswith((".tar.bz2", ".tbz2")):
        return ArtifactKind.TAR_BZIP2
    if lower.endswith(".tar"):
        return ArtifactKind.TAR
    if lower.endswith(".exe"):
        return ArtifactKind.WINDOWS_EXECUTABLE
    if lower.endswith(".msi"):
        return ArtifactKind.WINDOWS_INSTALLER
    if lower.endswith((".iso", ".img")):
        return ArtifactKind.DISK_IMAGE
    return None


def _validate_archive_name(value: str) -> str | None:
    normalized = value.replace("\\", "/")
    path = PurePosixPath(normalized)
    if normalized.startswith("/") or path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        return None
    if "\x00" in normalized:
        return None
    return str(path)
