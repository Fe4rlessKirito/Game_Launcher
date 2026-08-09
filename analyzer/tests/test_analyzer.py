from __future__ import annotations

import struct
from pathlib import Path

from launcher_analyzer import analyze_directory


def write_fake_pe(path: Path, machine: int = 0x8664) -> None:
    header = bytearray(512)
    header[:2] = b"MZ"
    struct.pack_into("<I", header, 0x3C, 0x80)
    header[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<H", header, 0x84, machine)
    path.write_bytes(header + b"x" * 2048)


def test_analyzer_ranks_game_executable_and_detects_metadata(tmp_path: Path) -> None:
    (tmp_path / "Game" / "Binaries").mkdir(parents=True)
    write_fake_pe(tmp_path / "Game" / "Binaries" / "ExampleGame.exe")
    write_fake_pe(tmp_path / "Launcher.exe", machine=0x014C)
    (tmp_path / "steam_appid.txt").write_text("480\n", encoding="utf-8")
    (tmp_path / "UnityPlayer.dll").write_bytes(b"unity")

    report = analyze_directory(tmp_path)

    assert report.schema_version == 1
    assert report.executables[0].path == "Game/Binaries/ExampleGame.exe"
    assert report.executables[0].architecture.value == "x64"
    assert report.app_id_candidates == ["480"]
    assert report.frameworks == ["Unity"]
    assert report.warnings == []


def test_report_order_is_deterministic(tmp_path: Path) -> None:
    write_fake_pe(tmp_path / "b.exe")
    write_fake_pe(tmp_path / "a.exe")
    first = analyze_directory(tmp_path).to_json()
    second = analyze_directory(tmp_path).to_json()
    assert first == second
