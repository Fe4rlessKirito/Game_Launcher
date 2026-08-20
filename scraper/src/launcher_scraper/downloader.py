from __future__ import annotations

import hashlib
import http.client
import os
import re
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from email.message import Message
from email.utils import parsedate_to_datetime
from pathlib import Path
from threading import Lock
from typing import Any
from urllib.parse import urljoin, urlparse

from blake3 import blake3

from .models import DownloadCandidate
from .security import DomainRateLimiter, URLPolicy, safe_filename


class DownloadError(RuntimeError):
    def __init__(
        self, message: str, *, retryable: bool = False, status: int | None = None, retry_after: float | None = None
    ) -> None:
        super().__init__(message)
        self.retryable = retryable
        self.status = status
        self.retry_after = retry_after


@dataclass(frozen=True)
class DownloadResult:
    path: str
    filename: str
    final_url: str
    status: int
    headers: dict[str, str]
    redirect_chain: tuple[str, ...]
    actual_size: int
    blake3: str
    sha256: str


DownloadProgressCallback = Callable[[int, int | None], None]


@dataclass(frozen=True)
class RetryPolicy:
    max_attempts: int = 3
    initial_backoff_seconds: float = 1.0
    max_backoff_seconds: float = 30.0


class ScratchBudget:
    def __init__(self, max_bytes: int) -> None:
        if max_bytes < 1:
            raise ValueError("max_bytes must be positive")
        self.max_bytes = max_bytes
        self._used = 0
        self._lock = Lock()

    @property
    def used_bytes(self) -> int:
        with self._lock:
            return self._used

    def reserve(self, amount: int) -> ScratchReservation:
        if amount < 0:
            raise ValueError("scratch reservation cannot be negative")
        self._reserve(amount)
        return ScratchReservation(self, amount)

    def _reserve(self, amount: int) -> None:
        with self._lock:
            if self._used + amount > self.max_bytes:
                raise DownloadError("temporary storage budget exhausted", retryable=True)
            self._used += amount

    def _release(self, amount: int) -> None:
        with self._lock:
            self._used = max(0, self._used - amount)


class ScratchReservation:
    def __init__(self, budget: ScratchBudget, amount: int) -> None:
        self.budget = budget
        self.amount = amount
        self.released = False

    def release(self) -> None:
        if not self.released:
            self.released = True
            self.budget._release(self.amount)
            self.amount = 0

    def resize(self, amount: int) -> None:
        if self.released:
            raise RuntimeError("scratch reservation has already been released")
        if amount < 0:
            raise ValueError("scratch reservation cannot be negative")
        if amount > self.amount:
            self.budget._reserve(amount - self.amount)
        elif amount < self.amount:
            self.budget._release(self.amount - amount)
        self.amount = amount

    def grow(self, amount: int) -> None:
        if amount > self.amount:
            self.resize(amount)

    def __enter__(self) -> ScratchReservation:
        return self

    def __exit__(self, *_: object) -> None:
        self.release()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_: object) -> None:
        return None


def _retry_after(headers: Message) -> float | None:
    value = headers.get("Retry-After")
    if not value:
        return None
    try:
        return max(0.0, min(300.0, float(value)))
    except ValueError:
        try:
            date = parsedate_to_datetime(value)
            return max(0.0, min(300.0, date.timestamp() - time.time()))
        except (TypeError, ValueError, OverflowError):
            return None


def _content_disposition_filename(value: str | None) -> str | None:
    if not value:
        return None
    match = re.search(r"filename\*?=(?:UTF-8''|\"|')?([^\"';]+)", value, flags=re.I)
    return safe_filename(match.group(1) if match else None) if match else None


def _parse_content_range(value: str | None) -> tuple[int, int | None] | None:
    if not value:
        return None
    match = re.fullmatch(r"bytes\s+(\d+)-(\d+)/(\d+|\*)", value.strip(), flags=re.I)
    if not match:
        return None
    return int(match.group(1)), None if match.group(3) == "*" else int(match.group(3))


class HttpDownloader:
    def __init__(
        self,
        *,
        policy: URLPolicy | None = None,
        rate_limiter: DomainRateLimiter | None = None,
        scratch_budget: ScratchBudget | None = None,
        user_agent: str = "VaultnodeScraper/0.1 (+server-side ingestion)",
        chunk_bytes: int = 1024 * 1024,
        timeout_seconds: float = 120.0,
        max_artifact_bytes: int = 512 * 1024**3,
    ) -> None:
        self.policy = policy or URLPolicy()
        self.rate_limiter = rate_limiter or DomainRateLimiter()
        self.scratch_budget = scratch_budget
        self.user_agent = user_agent
        self.chunk_bytes = max(16 * 1024, chunk_bytes)
        self.timeout_seconds = timeout_seconds
        self.max_artifact_bytes = max_artifact_bytes
        if self.max_artifact_bytes < 1:
            raise ValueError("max_artifact_bytes must be positive")
        self._opener = urllib.request.build_opener(_NoRedirect())

    def download(
        self,
        candidate: DownloadCandidate,
        destination_dir: str | Path,
        *,
        expected_size: int | None = None,
        expected_checksum: str | None = None,
        minimum_request_interval_seconds: float = 1.0,
        max_concurrent_requests: int = 2,
        retry_policy: RetryPolicy | None = None,
        extra_headers: dict[str, str] | None = None,
        progress: DownloadProgressCallback | None = None,
    ) -> DownloadResult:
        policy = retry_policy or RetryPolicy()
        if policy.max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        if expected_size is not None and not 0 < expected_size <= self.max_artifact_bytes:
            raise DownloadError("reported artifact size is outside configured bounds")
        destination = Path(destination_dir)
        destination.mkdir(parents=True, exist_ok=True)
        filename = safe_filename(candidate.filename or urlparse(candidate.url).path.rsplit("/", 1)[-1])
        final_path = destination / filename
        part_path = destination / f".{filename}.part"
        # A missing Content-Length is normal for redirectors and chunked responses.
        # Reserve those artifacts as they arrive instead of reserving the entire
        # configured maximum up front, which would reject every small unknown-size
        # download when the scratch pool is smaller than the global safety limit.
        reservation_amount = expected_size if expected_size is not None else 0
        reservation = self.scratch_budget.reserve(reservation_amount) if self.scratch_budget else None
        last_error: DownloadError | None = None
        try:
            for attempt in range(1, policy.max_attempts + 1):
                try:
                    return self._download_once(
                        candidate,
                        final_path,
                        part_path,
                        expected_size=expected_size,
                        expected_checksum=expected_checksum,
                        minimum_request_interval_seconds=minimum_request_interval_seconds,
                        max_concurrent_requests=max_concurrent_requests,
                        extra_headers=extra_headers,
                        progress=progress,
                        reservation=reservation,
                    )
                except DownloadError as error:
                    last_error = error
                    if not error.retryable or attempt >= policy.max_attempts:
                        raise
                    delay = error.retry_after
                    if delay is None:
                        delay = min(policy.max_backoff_seconds, policy.initial_backoff_seconds * (2 ** (attempt - 1)))
                    time.sleep(max(0.0, delay))
            raise last_error or DownloadError("download failed")
        finally:
            if reservation is not None:
                reservation.release()

    def _download_once(
        self,
        candidate: DownloadCandidate,
        final_path: Path,
        part_path: Path,
        *,
        expected_size: int | None,
        expected_checksum: str | None,
        minimum_request_interval_seconds: float,
        max_concurrent_requests: int,
        extra_headers: dict[str, str] | None,
        progress: DownloadProgressCallback | None,
        reservation: ScratchReservation | None,
    ) -> DownloadResult:
        url = self.policy.validate(candidate.url)
        domain = urlparse(url).hostname or ""
        offset = part_path.stat().st_size if part_path.exists() else 0
        if offset > self.max_artifact_bytes or (expected_size is not None and offset > expected_size):
            part_path.unlink(missing_ok=True)
            offset = 0
        if reservation is not None and expected_size is None:
            reservation.resize(offset)
        headers = {"User-Agent": self.user_agent, "Accept": "*/*"}
        if extra_headers:
            headers.update(extra_headers)
        if offset:
            headers["Range"] = f"bytes={offset}-"
        permit = self.rate_limiter.acquire(domain, minimum_request_interval_seconds, max_concurrent_requests)
        try:
            return self._download_with_response(
                url,
                final_path,
                part_path,
                headers,
                expected_size=expected_size,
                expected_checksum=expected_checksum,
                offset=offset,
                progress=progress,
                reservation=reservation,
            )
        finally:
            permit.release()

    def _download_with_response(
        self,
        url: str,
        final_path: Path,
        part_path: Path,
        headers: dict[str, str],
        *,
        expected_size: int | None,
        expected_checksum: str | None,
        offset: int,
        progress: DownloadProgressCallback | None,
        reservation: ScratchReservation | None,
    ) -> DownloadResult:
        response, final_url, redirects = self._open_with_redirects(url, headers)
        status = int(getattr(response, "status", 200))
        response_headers = {key.casefold(): value for key, value in response.headers.items()}
        retryable_status = status == 429 or 500 <= status <= 599
        if retryable_status:
            retry_after = _retry_after(response.headers)
            response.close()
            raise DownloadError(
                f"download returned HTTP {status}", retryable=True, status=status, retry_after=retry_after
            )
        if status < 200 or status >= 300:
            response.close()
            raise DownloadError(f"download returned HTTP {status}", status=status)

        content_range = _parse_content_range(response_headers.get("content-range"))
        append = bool(offset and status == 206 and content_range and content_range[0] == offset)
        if offset and not append:
            offset = 0
            if reservation is not None and expected_size is None:
                reservation.resize(0)
        mode = "ab" if append else "wb"
        written = offset
        max_bytes = self.max_artifact_bytes
        content_length = response_headers.get("content-length")
        total_size = expected_size
        if content_range and content_range[1] is not None:
            total_size = content_range[1]
        elif content_length:
            try:
                total_size = int(content_length) + (offset if append else 0)
            except ValueError:
                pass
        if content_length:
            try:
                advertised = int(content_length) + offset
                if advertised > max_bytes or (expected_size is not None and advertised > expected_size):
                    response.close()
                    raise DownloadError("download exceeds the configured artifact size", status=status)
                if reservation is not None and expected_size is None:
                    try:
                        reservation.grow(advertised)
                    except (DownloadError, ValueError, RuntimeError):
                        response.close()
                        raise
            except ValueError:
                pass
        if progress is not None:
            progress(written, total_size)
        try:
            with response, part_path.open(mode) as output:
                while True:
                    chunk = response.read(self.chunk_bytes)
                    if not chunk:
                        break
                    written += len(chunk)
                    if written > max_bytes or (expected_size is not None and written > expected_size):
                        raise DownloadError("download exceeded the configured artifact size", status=status)
                    if reservation is not None and expected_size is None:
                        reservation.grow(written)
                    output.write(chunk)
                    if progress is not None:
                        progress(written, total_size)
                output.flush()
                os.fsync(output.fileno())
        except DownloadError:
            raise
        except (OSError, http.client.HTTPException, urllib.error.URLError) as error:
            raise DownloadError(f"streaming download failed: {error}", retryable=True, status=status) from error

        if written == 0:
            raise DownloadError("downloaded artifact is empty")
        if expected_size is not None and written != expected_size:
            raise DownloadError(
                f"download size mismatch: expected {expected_size}, got {written}",
                retryable=True,
                status=status,
            )
        digest_blake3, digest_sha256 = _hash_file(part_path)
        if expected_checksum and not _checksum_matches(expected_checksum, digest_blake3, digest_sha256):
            part_path.unlink(missing_ok=True)
            raise DownloadError("reported checksum does not match downloaded artifact")
        final_path.unlink(missing_ok=True)
        part_path.replace(final_path)
        response_filename = _content_disposition_filename(response_headers.get("content-disposition"))
        return DownloadResult(
            str(final_path),
            response_filename or final_path.name,
            final_url,
            status,
            response_headers,
            tuple(redirects),
            written,
            digest_blake3,
            digest_sha256,
        )

    def _open_with_redirects(self, url: str, headers: dict[str, str]) -> tuple[Any, str, list[str]]:
        current = url
        redirects: list[str] = []
        for _ in range(self.policy.max_redirects + 1):
            request = urllib.request.Request(current, headers=headers, method="GET")
            try:
                response = self._opener.open(request, timeout=self.timeout_seconds)
            except urllib.error.HTTPError as error:
                if error.code not in {301, 302, 303, 307, 308}:
                    raise DownloadError(
                        f"download returned HTTP {error.code}",
                        retryable=error.code == 429 or error.code >= 500,
                        status=error.code,
                        retry_after=_retry_after(error.headers),
                    ) from error
                response = error
            except urllib.error.URLError as error:
                raise DownloadError(f"download request failed: {error.reason}", retryable=True) from error
            status = int(getattr(response, "status", response.code if hasattr(response, "code") else 200))
            if status not in {301, 302, 303, 307, 308}:
                return response, current, redirects
            location = response.headers.get("Location")
            response.close()
            if not location:
                raise DownloadError(f"redirect {status} did not include Location")
            next_url = urljoin(current, location)
            self.policy.validate_redirect(current, next_url)
            if (urlparse(next_url).hostname or "").casefold() != (urlparse(current).hostname or "").casefold():
                headers.pop("Cookie", None)
                headers.pop("Authorization", None)
            redirects.append(next_url)
            current = next_url
        raise DownloadError(f"download exceeded {self.policy.max_redirects} redirects")


def _hash_file(path: Path) -> tuple[str, str]:
    digest = blake3()
    sha = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            sha.update(chunk)
    return digest.hexdigest(), sha.hexdigest()


def _checksum_matches(value: str, blake3_value: str, sha256_value: str) -> bool:
    normalized = value.strip().casefold()
    if ":" in normalized:
        algorithm, normalized = normalized.split(":", 1)
        if algorithm in {"sha256", "sha-256"}:
            return normalized == sha256_value
        if algorithm == "blake3":
            return normalized == blake3_value
    return normalized in {blake3_value, sha256_value}
