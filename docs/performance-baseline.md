# Local performance baseline

Run date: 2026-08-09. This is a regression baseline for one developer machine and one small synthetic fixture, not a general performance claim.

Environment:

- Windows 11 Pro 10.0.26200 (build 26200), x64
- AMD Ryzen 5 7600X, 6 cores / 6 logical processors, reported 4,701 MHz
- 33,378,181,120 bytes physical memory (about 31.1 GiB)
- .NET SDK 10.0.302 / runtime 10.0.10 from the repository-local `.dotnet` runtime
- Rust 1.96.0, Cargo 1.96.0
- Python 3.14.5

Command:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-local-e2e.ps1 -SkipBuild
```

The script records phase timings in `artifacts/e2e/run/metrics.json`:

| Measurement | Observed |
|---|---:|
| Rust API health/startup to healthy | 307.925 ms |
| Cold synthetic Build A install phase | 954.534 ms for 8,475,285 encoded bytes |
| Build A -> B update phase | 697.010 ms; 1,396,521 network bytes; 8,127,524 cache bytes |
| Repair phase | 590.485 ms; 0 network bytes; 9,524,045 cache bytes |
| Full local script wall time (fixture generation, packaging, signing, publishing, API, install/update/repair) | 19,500.694 ms |

Local API request samples from the same run were 1.788 ms for catalog listing, 1.621 ms for a manifest fetch, and 1.789 ms for a four-chunk resolution request.

Avalonia desktop smoke validation launched the built `Launcher.App.exe`, observed a real main-window handle on both consecutive runs, and terminated the process cleanly after sampling it:

| Run | Window-ready time | Idle working set |
|---|---:|---:|
| first observed run | 156.529 ms | 21,950,464 bytes |
| second observed run | 242.440 ms | 21,934,080 bytes |

The ViewModel suite also exercises Home -> Library -> Downloads -> Settings -> Game Details navigation. This is a desktop smoke check, not a headless binding-log or accessibility certification.

Additional observed results:

- Build A: 8,570,372 raw bytes, 8,475,285 encoded bytes, 34 chunks.
- Build B: 9,619,085 raw bytes, 9,524,045 encoded bytes, 36 chunks.
- Update network savings: 85.33689204534418%.
- Update installed-file reuse: 4,357,397 bytes; reconstructed from verified cache/chunks: 5,261,688 bytes.
- Repair cache reuse: 9,524,045 bytes across 36 chunks.

The E2E run does not instrument cold/warm Avalonia window readiness, idle GUI RSS, peak installer RSS, or interactive navigation latency. Those remain release follow-ups; the E2E timings above include process startup, HTTP, cache, reconstruction, and filesystem work and must not be interpreted as isolated subsystem throughput. The 256-chunk downloader stress test validates bounded concurrency but is not a sustained network throughput benchmark.

The dedicated streaming harness was then run with a generated 512 MiB deterministic file:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\run-local-baseline.ps1 -SizeMiB 512
```

Observed subsystem measurements:

| Measurement | Observed |
|---|---:|
| BLAKE3 streaming hash of 536,870,912 bytes | 149.578 ms (about 3.59 GiB/s) |
| Zstandard level 3 compression | 404.468 ms; 536,883,209 encoded bytes |
| Zstandard streaming decompression | 263.011 ms (about 1.90 GiB/s) |
| Packager: 536,870,945 raw bytes / 1,670 chunks | 10,883.565 ms |
| Packager process peak working set | 13,070,336 bytes; admin process only |

The zero-filled multi-GiB streaming run was:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\run-large-streaming.ps1
```

It processed 4,294,967,296 input bytes in 25,397.360 ms, emitted 4,097 chunks, and reported 13,639,680 bytes peak working set for the admin process. Because zero-filled data compresses unusually well (204,842 encoded bytes and 2 unique objects), this run is a memory/streaming bound, not a realistic compression-throughput sample. Peak working set excludes the child Python analyzer and is therefore not a total process-tree RSS measurement. A production release benchmark should sample the complete process tree and include representative encrypted/compressed game assets.
