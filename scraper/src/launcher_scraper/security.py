from __future__ import annotations

import ipaddress
import re
import socket
import threading
import time
from dataclasses import dataclass
from pathlib import PurePosixPath
from urllib.parse import unquote, urlparse


class UrlPolicyError(ValueError):
    pass


def _is_blocked_ip(value: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    if value.is_loopback or value.is_private or value.is_link_local or value.is_multicast or value.is_unspecified:
        return True
    if value.is_reserved:
        return True
    if isinstance(value, ipaddress.IPv4Address):
        return value in ipaddress.IPv4Network("100.64.0.0/10") or value in ipaddress.IPv4Network("169.254.0.0/16")
    return False


@dataclass(frozen=True)
class URLPolicy:
    allow_localhost: bool = False
    resolve_dns: bool = True
    max_redirects: int = 8
    allowed_schemes: tuple[str, ...] = ("http", "https")

    def validate(self, url: str) -> str:
        parsed = urlparse(url)
        if parsed.scheme.casefold() not in self.allowed_schemes:
            raise UrlPolicyError(f"unsupported URL scheme: {parsed.scheme or '<missing>'}")
        if not parsed.hostname or parsed.username or parsed.password:
            raise UrlPolicyError("URL must contain a hostname and no embedded credentials")
        try:
            port = parsed.port
        except ValueError as error:
            raise UrlPolicyError("URL port is outside the valid range") from error
        host = parsed.hostname.rstrip(".").casefold()
        if host in {"metadata.google.internal", "metadata", "instance-data"}:
            raise UrlPolicyError(f"blocked metadata hostname: {host}")
        if host in {"localhost", "localhost.localdomain"} and not self.allow_localhost:
            raise UrlPolicyError(f"blocked local hostname: {host}")
        try:
            literal = ipaddress.ip_address(host)
        except ValueError:
            literal = None
        if literal is not None:
            if _is_blocked_ip(literal) and not (self.allow_localhost and literal.is_loopback):
                raise UrlPolicyError(f"blocked private or local address: {host}")
        elif self.resolve_dns:
            try:
                addresses = {
                    ipaddress.ip_address(result[4][0])
                    for result in socket.getaddrinfo(
                        host, port or (443 if parsed.scheme == "https" else 80), type=socket.SOCK_STREAM
                    )
                }
            except OSError as error:
                raise UrlPolicyError(f"could not resolve public hostname {host}: {error}") from error
            blocked = next((address for address in addresses if _is_blocked_ip(address)), None)
            if blocked is not None and not (
                self.allow_localhost and addresses and all(address.is_loopback for address in addresses)
            ):
                raise UrlPolicyError(f"hostname resolves to blocked address: {host} -> {blocked}")
        if port is not None and not 1 <= port <= 65535:
            raise UrlPolicyError("URL port is outside the valid range")
        return url

    def validate_redirect(self, previous_url: str, next_url: str) -> str:
        return self.validate(next_url)


def safe_filename(value: str | None, fallback: str = "download.bin") -> str:
    raw = unquote(value or "").replace("\\", "/")
    name = PurePosixPath(raw).name
    name = re.sub(r"[\x00-\x1f\x7f<>:\"/|?*]", "_", name).strip(" .")
    if name in {"", ".", ".."}:
        name = fallback
    if len(name) > 180:
        stem, dot, suffix = name.rpartition(".")
        name = stem[: max(1, 180 - len(suffix) - (1 if dot else 0))] + (dot + suffix if dot else "")
    return name


class DomainRateLimiter:
    """Small in-process per-domain politeness gate shared by browser/download paths."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._next_allowed: dict[str, float] = {}
        self._semaphores: dict[str, threading.BoundedSemaphore] = {}

    def acquire(self, domain: str, minimum_interval: float, max_concurrent: int) -> _DomainPermit:
        key = domain.casefold()
        with self._lock:
            semaphore = self._semaphores.get(key)
            if semaphore is None:
                semaphore = threading.BoundedSemaphore(max(1, min(32, max_concurrent)))
                self._semaphores[key] = semaphore
        semaphore.acquire()
        now = time.monotonic()
        with self._lock:
            wait = max(0.0, self._next_allowed.get(key, 0.0) - now)
            self._next_allowed[key] = max(now, self._next_allowed.get(key, 0.0)) + max(0.0, minimum_interval)
        if wait:
            time.sleep(wait)
        return _DomainPermit(semaphore)


class _DomainPermit:
    def __init__(self, semaphore: threading.BoundedSemaphore) -> None:
        self._semaphore = semaphore
        self._released = False

    def release(self) -> None:
        if not self._released:
            self._released = True
            self._semaphore.release()

    def __enter__(self) -> _DomainPermit:
        return self

    def __exit__(self, *_: object) -> None:
        self.release()
