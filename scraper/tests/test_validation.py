from __future__ import annotations

import io
import zipfile

from launcher_scraper.models import ArtifactKind, DownloadCandidate
from launcher_scraper.validation import ArtifactValidator, DedupIndex


def _candidate(path: str = "game.zip", **kwargs: object) -> DownloadCandidate:
    return DownloadCandidate(
        url=f"https://example.test/{path}",
        label="Download",
        filename=path,
        confidence=1.0,
        **kwargs,
    )


def _zip_bytes(name: str = "game/readme.txt", value: bytes = b"hello") -> bytes:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(name, value)
    return stream.getvalue()


def test_validator_accepts_valid_zip_and_rejects_html(tmp_path) -> None:
    valid = tmp_path / "game.zip"
    valid.write_bytes(_zip_bytes())
    result = ArtifactValidator().validate(valid, _candidate(reported_size=valid.stat().st_size))
    assert result.ok
    assert result.kind == ArtifactKind.ZIP
    assert result.archive_entries == 1

    html = tmp_path / "fake.zip"
    html.write_text("<!doctype html><html>login</html>", encoding="utf-8")
    rejected = ArtifactValidator().validate(html, _candidate("fake.zip"))
    assert not rejected.ok
    assert any("HTML" in error or "html" in error for error in rejected.errors)


def test_validator_rejects_zip_traversal_and_crc_errors(tmp_path) -> None:
    traversal = tmp_path / "traversal.zip"
    traversal.write_bytes(_zip_bytes("../escape.txt"))
    rejected = ArtifactValidator().validate(traversal, _candidate("traversal.zip"))
    assert not rejected.ok
    assert any("unsafe archive path" in error for error in rejected.errors)

    corrupt = tmp_path / "corrupt.zip"
    data = bytearray(_zip_bytes())
    data[-10] ^= 0xFF
    corrupt.write_bytes(data)
    corrupt_result = ArtifactValidator().validate(corrupt, _candidate("corrupt.zip"))
    assert not corrupt_result.ok

    wrong_magic = tmp_path / "wrong.zip"
    wrong_magic.write_bytes(b"not a zip")
    wrong_magic_result = ArtifactValidator().validate(wrong_magic, _candidate("wrong.zip"))
    assert not wrong_magic_result.ok
    assert any("magic bytes" in error for error in wrong_magic_result.errors)


def test_dedup_index_is_durable(tmp_path) -> None:
    index = DedupIndex(tmp_path / "dedup.json")
    index.record("abc", {"path": "artifact.zip"})
    restored = DedupIndex(tmp_path / "dedup.json")
    assert restored.lookup("abc") == {"path": "artifact.zip"}
