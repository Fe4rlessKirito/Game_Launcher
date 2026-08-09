from __future__ import annotations

import json
import re
import struct
from dataclasses import asdict, dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
MAX_HEADER_BYTES = 64 * 1024


class Architecture(StrEnum):
    X86 = "x86"
    X64 = "x64"
    ARM64 = "arm64"
    UNKNOWN = "unknown"


class FindingKind(StrEnum):
    EXECUTABLE = "executable"
    FRAMEWORK = "framework"
    APP_ID = "app_id"
    PREREQUISITE = "prerequisite"
    WARNING = "warning"


@dataclass(frozen=True)
class Finding:
    kind: FindingKind
    value: str
    confidence: float
    evidence: tuple[str, ...] = ()


@dataclass(frozen=True)
class ExecutableCandidate:
    path: str
    architecture: Architecture
    size: int
    score: int
    evidence: tuple[str, ...] = ()


@dataclass
class AnalysisReport:
    schema_version: int
    source_directory: str
    scanned_files: int
    total_bytes: int
    executables: list[ExecutableCandidate]
    frameworks: list[str]
    app_id_candidates: list[str]
    prerequisites: list[str]
    findings: list[Finding] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), indent=2, sort_keys=True) + "\n"


def analyze_directory(directory: Path) -> AnalysisReport:
    root = directory.expanduser().resolve()
    if not root.is_dir():
        raise NotADirectoryError(root)

    executable_candidates: list[ExecutableCandidate] = []
    findings: list[Finding] = []
    frameworks: set[str] = set()
    app_ids: set[str] = set()
    prerequisites: set[str] = set()
    warnings: list[str] = []
    scanned_files = 0
    total_bytes = 0

    try:
        paths = sorted(
            (path for path in root.rglob("*") if path.is_file()),
            key=lambda path: path.relative_to(root).as_posix().casefold(),
        )
    except OSError as error:
        paths = []
        warnings.append(f"directory scan warning: {error}")

    for path in paths:
        portable = path.relative_to(root).as_posix()
        if path.is_symlink():
            warnings.append(f"symlink skipped: {portable}")
            findings.append(
                Finding(FindingKind.WARNING, portable, 1.0, ("symbolic link is outside the owned build input",))
            )
            continue
        try:
            size = path.stat().st_size
            scanned_files += 1
            total_bytes += size
        except OSError as error:
            warnings.append(f"metadata unavailable for {portable}: {error}")
            findings.append(Finding(FindingKind.WARNING, portable, 1.0, (str(error),)))
            continue

        lower_name = path.name.casefold()
        if lower_name in {"unityplayer.dll", "gameassembly.dll"} or "unity" in lower_name:
            frameworks.add("Unity")
        if lower_name.startswith(("ue4", "ue5", "unreal")) or lower_name == "unrealengine.dll":
            frameworks.add("Unreal Engine")
        if lower_name.endswith(".runtimeconfig.json") or lower_name.endswith(".deps.json"):
            frameworks.add(".NET")
        if lower_name in {"dxgi.dll", "d3d12.dll", "d3d11.dll"}:
            prerequisites.add("DirectX")
        if lower_name in {"vcruntime140.dll", "msvcp140.dll"}:
            prerequisites.add("Microsoft Visual C++ Runtime")

        if path.suffix.casefold() == ".exe":
            architecture, pe_evidence = _read_pe_architecture(path)
            score, evidence = _rank_executable(path, root, size, architecture)
            executable_candidates.append(
                ExecutableCandidate(portable, architecture, size, score, tuple(pe_evidence + evidence))
            )

        if lower_name in {"steam_api.dll", "steam_api64.dll", "steamclient.dll"}:
            findings.append(Finding(FindingKind.FRAMEWORK, "Steamworks", 0.9, (f"adjacent file {portable}",)))
        if any(token in lower_name for token in ("eos_sdk", "eos-overlay", "epiconlineservices")):
            frameworks.add("Epic Online Services")
            findings.append(
                Finding(FindingKind.FRAMEWORK, "Epic Online Services", 0.8, (f"indicator file {portable}",))
            )
        if "playfab" in lower_name:
            frameworks.add("PlayFab")
            findings.append(Finding(FindingKind.FRAMEWORK, "PlayFab", 0.8, (f"indicator file {portable}",)))
        if (
            lower_name in {"easyanticheat.sys", "easyanticheat_x64.dll", "easyanticheat_launcher.exe"}
            or "easyanticheat" in lower_name
        ):
            prerequisites.add("Easy Anti-Cheat")
            findings.append(Finding(FindingKind.PREREQUISITE, "Easy Anti-Cheat", 0.9, (f"indicator file {portable}",)))
        if lower_name in {"battleye.dll", "beservice.exe", "beservice_x64.exe"} or "battleye" in lower_name:
            prerequisites.add("BattlEye")
            findings.append(Finding(FindingKind.PREREQUISITE, "BattlEye", 0.9, (f"indicator file {portable}",)))

        if lower_name == "steam_appid.txt":
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
                found = re.findall(r"\b\d{3,12}\b", text)
                app_ids.update(found)
                for app_id in found:
                    findings.append(Finding(FindingKind.APP_ID, app_id, 1.0, (f"steam_appid.txt at {portable}",)))
            except OSError as error:
                warnings.append(f"could not read {portable}: {error}")
        match = re.fullmatch(r"appmanifest_(\d+)\.acf", lower_name)
        if match:
            app_ids.add(match.group(1))
            findings.append(Finding(FindingKind.APP_ID, match.group(1), 0.95, (f"Steam ACF filename {portable}",)))

        if lower_name.endswith(("crashreportclient.exe", "crashreporter.exe")) or "crashreport" in lower_name:
            findings.append(
                Finding(FindingKind.WARNING, "crash reporter candidate", 0.95, (f"utility filename {portable}",))
            )
        if lower_name in {"dotnet", "dotnet.exe", "vc_redist.x64.exe", "vc_redist.x86.exe"}:
            prerequisites.add("runtime installer")
            findings.append(
                Finding(FindingKind.PREREQUISITE, "runtime installer", 0.8, (f"prerequisite filename {portable}",))
            )

    executable_candidates.sort(key=lambda candidate: (-candidate.score, candidate.path.casefold()))
    for candidate in executable_candidates:
        findings.append(
            Finding(
                FindingKind.EXECUTABLE, candidate.path, min(1.0, max(0.0, candidate.score / 100)), candidate.evidence
            )
        )
    for framework in sorted(frameworks):
        if not any(finding.kind == FindingKind.FRAMEWORK and finding.value == framework for finding in findings):
            framework_evidence = tuple(_framework_evidence(framework, paths, root))
            findings.append(Finding(FindingKind.FRAMEWORK, framework, 0.9, framework_evidence))
    for app_id in sorted(app_ids):
        if not any(finding.kind == FindingKind.APP_ID and finding.value == app_id for finding in findings):
            findings.append(Finding(FindingKind.APP_ID, app_id, 0.85, ("app ID metadata",)))
    for prerequisite in sorted(prerequisites):
        if not any(finding.kind == FindingKind.PREREQUISITE and finding.value == prerequisite for finding in findings):
            findings.append(Finding(FindingKind.PREREQUISITE, prerequisite, 0.7, ("prerequisite metadata",)))
    if len(app_ids) > 1:
        warnings.append(f"conflicting AppID candidates: {', '.join(sorted(app_ids))}")
        findings.append(Finding(FindingKind.WARNING, "conflicting AppID candidates", 0.9, tuple(sorted(app_ids))))
    if not executable_candidates:
        warnings.append("no executable candidates found")
        findings.append(Finding(FindingKind.WARNING, "no executable", 1.0, ("no .exe files found in the build",)))

    return AnalysisReport(
        schema_version=SCHEMA_VERSION,
        source_directory=str(root),
        scanned_files=scanned_files,
        total_bytes=total_bytes,
        executables=executable_candidates,
        frameworks=sorted(frameworks),
        app_id_candidates=sorted(app_ids),
        prerequisites=sorted(prerequisites),
        findings=findings,
        warnings=sorted(warnings),
    )


def _rank_executable(path: Path, root: Path, size: int, architecture: Architecture) -> tuple[int, list[str]]:
    relative = path.relative_to(root).as_posix().casefold()
    stem = path.stem.casefold()
    score = 20
    evidence: list[str] = []
    if architecture in {Architecture.X64, Architecture.X86, Architecture.ARM64}:
        score += 20
        evidence.append("valid PE header")
    if any(token in stem for token in ("game", "client", "app")):
        score += 25
        evidence.append("game-like filename")
    if any(token in relative for token in ("/binaries/", "/bin/", "/game/")):
        score += 20
        evidence.append("game binary directory")
    if any(token in stem for token in ("launcher", "setup", "uninstall", "crash", "updater")):
        score -= 25
        evidence.append("launcher/utility filename")
    if size > 1024 * 1024:
        score += 10
        evidence.append("substantial executable size")
    return score, evidence


def _read_pe_architecture(path: Path) -> tuple[Architecture, list[str]]:
    try:
        with path.open("rb") as file:
            header = file.read(MAX_HEADER_BYTES)
    except OSError as error:
        return Architecture.UNKNOWN, [f"header read failed: {error}"]
    if len(header) < 0x40 or header[:2] != b"MZ":
        return Architecture.UNKNOWN, ["not a valid DOS PE header"]
    pe_offset = struct.unpack_from("<I", header, 0x3C)[0]
    if pe_offset < 0x40 or pe_offset + 6 > len(header) or header[pe_offset : pe_offset + 4] != b"PE\0\0":
        return Architecture.UNKNOWN, ["missing PE signature"]
    machine = struct.unpack_from("<H", header, pe_offset + 4)[0]
    architecture = {0x014C: Architecture.X86, 0x8664: Architecture.X64, 0xAA64: Architecture.ARM64}.get(
        machine, Architecture.UNKNOWN
    )
    return architecture, [f"PE machine 0x{machine:04x}"]


def _framework_evidence(framework: str, paths: list[Path], root: Path) -> list[str]:
    evidence: list[str] = []
    for path in paths:
        name = path.name.casefold()
        portable = path.relative_to(root).as_posix()
        if framework == "Unity" and (name in {"unityplayer.dll", "gameassembly.dll"} or "unity" in name):
            evidence.append(f"{portable} matches Unity indicator")
        elif framework == "Unreal Engine" and (
            name.startswith(("ue4", "ue5", "unreal")) or "/engine/" in f"/{portable.casefold()}/"
        ):
            evidence.append(f"{portable} matches Unreal indicator")
        elif framework == ".NET" and name.endswith((".runtimeconfig.json", ".deps.json")):
            evidence.append(f"{portable} is a .NET runtime descriptor")
    return evidence or ["framework indicator metadata"]
