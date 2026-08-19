from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass, field
from html.parser import HTMLParser
from urllib.parse import urljoin, urlparse

from .models import (
    ElementTarget,
    PageSnapshot,
    SemanticButton,
    SemanticForm,
    SemanticLink,
)

_HIDDEN_TAGS = {"head", "script", "style", "template", "noscript", "svg", "canvas"}
_DOWNLOAD_WORDS = re.compile(r"\b(download|direct|installer|portable|mirror|archive|setup|release)\b", re.I)
_ARCHIVE_SUFFIXES = (".zip", ".7z", ".rar", ".tar", ".tgz", ".tar.gz", ".tar.bz2", ".exe", ".msi", ".iso")


def _normalize(value: str, limit: int = 2000) -> str:
    return re.sub(r"\s+", " ", value or "").strip()[:limit]


def _attrs(values: list[tuple[str, str | None]]) -> dict[str, str]:
    return {key.casefold(): value or "" for key, value in values}


@dataclass
class _Capture:
    tag: str
    identifier: str | None
    kind: str | None
    ordinal: int = 0
    attributes: dict[str, str] = field(default_factory=dict)
    text: list[str] = field(default_factory=list)
    fields: list[str] = field(default_factory=list)


@dataclass
class _Frame:
    tag: str
    hidden: bool
    breadcrumb: bool
    capture: _Capture | None = None


class _SemanticParser(HTMLParser):
    def __init__(self, base_url: str, max_text_bytes: int) -> None:
        super().__init__(convert_charrefs=True)
        self.base_url = base_url
        self.max_text_bytes = max_text_bytes
        self.frames: list[_Frame] = []
        self.hidden_depth = 0
        self.breadcrumb_depth = 0
        self.title_parts: list[str] = []
        self.visible_parts: list[str] = []
        self.headings: list[str] = []
        self.links: list[SemanticLink] = []
        self.buttons: list[SemanticButton] = []
        self.forms: list[SemanticForm] = []
        self.metadata: dict[str, str] = {}
        self.breadcrumb_parts: list[str] = []
        self.targets: list[ElementTarget] = []
        self._anchor_ordinal = 0
        self._button_ordinal = 0
        self._form_ordinal = 0
        self._heading_captures: list[_Capture] = []
        self._breadcrumb_current: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        tag = tag.casefold()
        values = _attrs(attrs)
        hidden = (
            self.hidden_depth > 0
            or tag in _HIDDEN_TAGS
            or "hidden" in values
            or values.get("aria-hidden", "").casefold() == "true"
            or "display:none" in values.get("style", "").replace(" ", "").casefold()
        )
        breadcrumb = self._is_breadcrumb(tag, values)
        if hidden:
            self.hidden_depth += 1
        if breadcrumb:
            self.breadcrumb_depth += 1

        capture: _Capture | None = None
        if tag == "title":
            capture = _Capture(tag, None, None)
        elif tag == "a":
            identifier = f"L{len(self.links) + 1}"
            self._anchor_ordinal += 1
            capture = _Capture(tag, identifier, "link", self._anchor_ordinal, values)
            self.targets.append(ElementTarget(identifier, "link", self._anchor_ordinal))
        elif tag == "button" or values.get("role", "").casefold() == "button":
            identifier = f"B{len(self.buttons) + 1}"
            self._button_ordinal += 1
            capture = _Capture(tag, identifier, "button", self._button_ordinal, values)
            self.targets.append(ElementTarget(identifier, "button", self._button_ordinal))
        elif tag == "form":
            identifier = f"F{len(self.forms) + 1}"
            self._form_ordinal += 1
            capture = _Capture(tag, identifier, "form", self._form_ordinal, values)
        elif tag in {"h1", "h2", "h3", "h4", "h5", "h6"}:
            capture = _Capture(tag, None, "heading")
            self._heading_captures.append(capture)

        if tag == "meta":
            key = values.get("property") or values.get("name") or values.get("itemprop")
            content = _normalize(values.get("content", ""), 1000)
            if key and content:
                self.metadata[key.casefold()] = content

        if tag in {"input", "select", "textarea"} and self.frames:
            name = values.get("name") or values.get("id") or values.get("type")
            if name:
                for frame in reversed(self.frames):
                    if frame.capture and frame.capture.kind == "form":
                        frame.capture.fields.append(name[:120])
                        break

        self.frames.append(_Frame(tag, hidden, breadcrumb, capture))

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_endtag(self, tag: str) -> None:
        tag = tag.casefold()
        index = next((index for index in range(len(self.frames) - 1, -1, -1) if self.frames[index].tag == tag), None)
        if index is None:
            return
        frames = self.frames[index:]
        self.frames = self.frames[:index]
        for frame in reversed(frames):
            self._finish_capture(frame.capture)
            if frame.hidden:
                self.hidden_depth = max(0, self.hidden_depth - 1)
            if frame.breadcrumb:
                self.breadcrumb_depth = max(0, self.breadcrumb_depth - 1)
                if self._breadcrumb_current:
                    text = _normalize(" ".join(self._breadcrumb_current))
                    if text:
                        self.breadcrumb_parts.append(text)
                    self._breadcrumb_current.clear()

    def handle_data(self, data: str) -> None:
        if not data:
            return
        normalized = _normalize(data, 4000)
        if not normalized:
            return
        if any(frame.tag == "title" for frame in self.frames):
            self.title_parts.append(normalized)
        if self.hidden_depth > 0:
            return
        current_bytes = sum(len(part.encode("utf-8")) for part in self.visible_parts)
        if current_bytes < self.max_text_bytes:
            self.visible_parts.append(normalized)
        if self.breadcrumb_depth > 0:
            self._breadcrumb_current.append(normalized)
        for frame in self.frames:
            if frame.capture:
                frame.capture.text.append(normalized)

    def _finish_capture(self, capture: _Capture | None) -> None:
        if capture is None:
            return
        text = _normalize(" ".join(capture.text))
        if capture.kind == "heading" and text:
            self.headings.append(text)
            if capture in self._heading_captures:
                self._heading_captures.remove(capture)
        elif capture.kind == "link":
            href = urljoin(self.base_url, capture.attributes.get("href", "").strip())
            if not href:
                return
            self.links.append(
                SemanticLink(
                    capture.identifier or "",
                    text,
                    href,
                    context=text,
                    rel=capture.attributes.get("rel", ""),
                    download=capture.attributes.get("download", ""),
                )
            )
        elif capture.kind == "button":
            self.buttons.append(SemanticButton(capture.identifier or "", text, context=text))
        elif capture.kind == "form":
            action = urljoin(self.base_url, capture.attributes.get("action", "") or self.base_url)
            self.forms.append(
                SemanticForm(
                    capture.identifier or "",
                    action,
                    capture.attributes.get("method", "get").casefold(),
                    tuple(dict.fromkeys(capture.fields)),
                )
            )

    @staticmethod
    def _is_breadcrumb(tag: str, values: dict[str, str]) -> bool:
        marker = " ".join((values.get("class", ""), values.get("id", ""), values.get("aria-label", ""))).casefold()
        return "breadcrumb" in marker or (tag == "nav" and "breadcrumb" in marker)


class SemanticDomBuilder:
    """Reduce untrusted HTML to a bounded, deterministic page representation."""

    def __init__(self, max_text_bytes: int = 24_000) -> None:
        if max_text_bytes < 1024:
            raise ValueError("max_text_bytes must be at least 1024")
        self.max_text_bytes = max_text_bytes

    def build(self, html: str, url: str, title: str | None = None) -> PageSnapshot:
        parser = _SemanticParser(url, self.max_text_bytes)
        parser.feed(html[: max(self.max_text_bytes * 8, 128_000)])
        parser.close()
        links = tuple(parser.links)
        downloads = tuple(
            link.id
            for link in links
            if _DOWNLOAD_WORDS.search(f"{link.text} {link.href} {link.download}")
            or urlparse(link.href).path.casefold().endswith(_ARCHIVE_SUFFIXES)
        )
        pagination = tuple(
            link.id
            for link in links
            if link.rel.casefold() in {"next", "prev", "previous"}
            or link.text.casefold() in {"next", "previous", "prev", "older", "newer"}
            or link.text.strip().isdigit()
        )
        metadata = dict(parser.metadata)
        resolved_title = _normalize(title or " ".join(parser.title_parts), 500) or metadata.get("og:title", "")
        visible_text = _normalize(" ".join(parser.visible_parts), self.max_text_bytes)
        compact = {
            "url": url,
            "title": resolved_title,
            "headings": parser.headings,
            "visible_text": visible_text,
            "links": [link.__dict__ for link in links],
            "buttons": [button.__dict__ for button in parser.buttons],
            "forms": [form.__dict__ for form in parser.forms],
            "downloads_detected": downloads,
            "breadcrumbs": tuple(dict.fromkeys(parser.breadcrumb_parts)),
            "pagination": pagination,
            "metadata": metadata,
        }
        state_hash = hashlib.sha256(
            json.dumps(compact, sort_keys=True, default=list, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        return PageSnapshot(
            url=url,
            title=resolved_title,
            headings=tuple(dict.fromkeys(parser.headings)),
            visible_text=visible_text,
            links=links,
            buttons=tuple(parser.buttons),
            forms=tuple(parser.forms),
            downloads_detected=downloads,
            breadcrumbs=tuple(dict.fromkeys(parser.breadcrumb_parts)),
            pagination=pagination,
            metadata=metadata,
            targets=tuple(parser.targets),
            state_hash=state_hash,
        )
