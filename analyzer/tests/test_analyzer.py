from __future__ import annotations

import struct
from pathlib import Path

import pytest

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


@pytest.mark.parametrize("machine,architecture", [(0x014C, "x86"), (0x8664, "x64"), (0xAA64, "arm64")])
def test_pe_architectures_are_reported_with_evidence(tmp_path: Path, machine: int, architecture: str) -> None:
    target = tmp_path / f"Game-{architecture}.exe"
    write_fake_pe(target, machine)
    report = analyze_directory(tmp_path)
    candidate = report.executables[0]
    assert candidate.architecture.value == architecture
    assert any("PE machine" in item for item in candidate.evidence)
    assert any(finding.evidence for finding in report.findings)


def test_unity_unreal_prerequisite_and_service_indicators_are_evidenced(tmp_path: Path) -> None:
    (tmp_path / "Game" / "Binaries" / "Win64").mkdir(parents=True)
    write_fake_pe(tmp_path / "Game" / "Binaries" / "Win64" / "ActualGame.exe")
    for name in [
        "UnityPlayer.dll",
        "GameAssembly.dll",
        "UE5Game-Win64-Shipping.exe",
        "steam_api64.dll",
        "eos_sdk.dll",
        "PlayFabParty.dll",
        "EasyAntiCheat_x64.dll",
        "BEService.exe",
    ]:
        (tmp_path / name).write_bytes(b"indicator")
    (tmp_path / "steam_appid.txt").write_text("480\n", encoding="utf-8")
    (tmp_path / "appmanifest_1234.acf").write_text('"appid" "1234"', encoding="utf-8")
    report = analyze_directory(tmp_path)
    assert "Unity" in report.frameworks
    assert "Unreal Engine" in report.frameworks
    assert "Epic Online Services" in report.frameworks
    assert "PlayFab" in report.frameworks
    assert "Easy Anti-Cheat" in report.prerequisites
    assert "BattlEye" in report.prerequisites
    assert report.app_id_candidates == ["1234", "480"]
    assert any(finding.value == "480" and finding.evidence for finding in report.findings)


def test_conflicting_app_ids_and_no_executable_are_explicit(tmp_path: Path) -> None:
    (tmp_path / "steam_appid.txt").write_text("480", encoding="utf-8")
    (tmp_path / "appmanifest_1234.acf").write_text("acf", encoding="utf-8")
    report = analyze_directory(tmp_path)
    assert report.executables == []
    assert "no executable candidates found" in report.warnings
    assert any("conflicting AppID" in warning for warning in report.warnings)
    assert all(finding.evidence for finding in report.findings)


def test_malformed_pe_is_unknown_but_scanning_continues(tmp_path: Path) -> None:
    (tmp_path / "bad.exe").write_bytes(b"MZ" + b"x" * 30)
    write_fake_pe(tmp_path / "good.exe")
    report = analyze_directory(tmp_path)
    candidates = {candidate.path: candidate for candidate in report.executables}
    assert candidates["bad.exe"].architecture.value == "unknown"
    assert candidates["good.exe"].architecture.value == "x64"


def test_symlink_is_reported_and_not_treated_as_owned_content(tmp_path: Path) -> None:
    target = tmp_path / "outside.exe"
    write_fake_pe(target)
    link = tmp_path / "linked.exe"
    try:
        link.symlink_to(target)
    except (OSError, NotImplementedError):
        pytest.skip("symlink creation is unavailable on this Windows runner")
    report = analyze_directory(tmp_path)
    assert any("symlink skipped" in warning for warning in report.warnings)
    assert all(candidate.path != "linked.exe" for candidate in report.executables)
