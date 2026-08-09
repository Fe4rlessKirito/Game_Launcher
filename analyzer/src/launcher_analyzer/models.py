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

        if lower_name == "steam_appid.txt":
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
                app_ids.update(re.findall(r"\b\d{3,12}\b", text))
            except OSError as error:
                warnings.append(f"could not read {portable}: {error}")
        match = re.fullmatch(r"appmanifest_(\d+)\.acf", lower_name)
        if match:
            app_ids.add(match.group(1))

    executable_candidates.sort(key=lambda candidate: (-candidate.score, candidate.path.casefold()))
    for candidate in executable_candidates:
        findings.append(
            Finding(
                FindingKind.EXECUTABLE, candidate.path, min(1.0, max(0.0, candidate.score / 100)), candidate.evidence
            )
        )
    for framework in sorted(frameworks):
        findings.append(Finding(FindingKind.FRAMEWORK, framework, 0.9))
    for app_id in sorted(app_ids):
        findings.append(Finding(FindingKind.APP_ID, app_id, 0.85))
    for prerequisite in sorted(prerequisites):
        findings.append(Finding(FindingKind.PREREQUISITE, prerequisite, 0.7))

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
    if pe_offset + 6 > len(header) or header[pe_offset : pe_offset + 4] != b"PE\0\0":
        return Architecture.UNKNOWN, ["missing PE signature"]
    machine = struct.unpack_from("<H", header, pe_offset + 4)[0]
    architecture = {0x014C: Architecture.X86, 0x8664: Architecture.X64, 0xAA64: Architecture.ARM64}.get(
        machine, Architecture.UNKNOWN
    )
    return architecture, [f"PE machine 0x{machine:04x}"]
