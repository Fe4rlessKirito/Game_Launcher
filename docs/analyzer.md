# Analyzer

`python -m launcher_analyzer analyze <directory> --output analysis.json --json` emits schema version 1. It reads bounded PE headers and metadata, ranks likely executables, records framework/runtime candidates, and scans known Steam/AppID metadata without modifying the source directory. Permission errors are per-file findings, not whole-run failures.

LIEF is optional at runtime because its binary wheels may lag a newly released Python. When installed, it enriches PE/ELF/Mach-O findings; the deterministic metadata scanner remains the fallback.
