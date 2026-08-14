# Implementation audit

Audit baseline: commit `da2aeea Build launcher platform foundation`.

This document is intentionally blunt. A project compiling is not evidence that a persistent boundary, integrity boundary, or failure path is complete.

| Subsystem | Status at audit | Test coverage at audit | Missing failure handling | Architectural risk |
|---|---|---|---|---|
| Manifest model and schema | Partially implemented. JSON v1 validation exists, but signatures and trusted key selection were absent. | Two Rust tests, no C# adversarial suite. | Null/malformed fields, reserved Windows names, normalized-path edge cases, duplicate/inconsistent chunk references. | A valid-looking unsigned manifest could be accepted by a client unless a caller added an external policy. |
| Rust packager | Implemented for local filesystem packaging. FastCDC, BLAKE3, and Zstandard are streamed/bounded by the maximum chunk size. | One CLI smoke path and basic manifest tests. | No fault injection, no deterministic boundary tests, no measured large-file RSS/throughput baseline. | Manifest timestamps/IDs are intentionally non-reproducible; upload/publication are separate stages and were only advanced in the worker's progress object. |
| Local storage | Implemented and hash-verifying on reads/writes. | One round-trip test. | Duplicate uploads, corrupt existing objects, deletion, missing objects, concurrent writers. | A shared `.part` name can collide under concurrent uploads; the provider had no delete operation. |
| PostgreSQL metadata | Scaffolding with real SQLx connection and one migration. | No live database tests; PostgreSQL was not installed or running on the audit machine. | Repository methods for chunks, locations, jobs, claiming, leasing, retry, and publication. | API falls back to an in-memory/local manifest catalog when PostgreSQL is absent. That mode is useful for development only and is not a production publication path. |
| Rust API | Implemented for catalog, manifest, resolve, health, and local object reads. | Local HTTP smoke test. | Build-aware chunk authorization, signature route, request logging policy, router-level tests. | The original resolve route returned URLs for any syntactically valid hash, not only chunks belonging to the requested build. |
| Python analyzer | Implemented deterministic bounded PE-header heuristics. | Two tests. | Fixture breadth, malformed/inaccessible files, evidence for every heuristic, symlink/large-tree behavior, Unity/Unreal/service indicators. | Heuristics are intentionally conservative and are not a substitute for a trusted installer policy. |
| .NET networking/downloader | Partially implemented. Parallel downloads, retries, mirrors, and cache verification exist. | Zstandard codec only; no HTTP behavior tests. | Resumable ranges, HTTP status classification, timeout/cancellation semantics, resolver refresh, partial-file persistence, metrics. | Each chunk was copied into a `MemoryStream`; a large encoded object could create avoidable memory pressure. |
| Chunk cache | Implemented as a bounded hash-verified directory cache. | No cache behavior tests. | Concurrent readers/writers, corrupt files, pinning, eviction, restart reconstruction of metadata. | Cache index is rebuilt from filenames and timestamps; an active download was not durable in the state database. |
| Installer | Partially implemented. Transactional staging, file and chunk verification, SQLite installed-game persistence, verify/repair/uninstall exist. | Four integration tests. | Update transaction, rollback after filesystem/DB split, disk-space preflight, stale journal recovery, obsolete-file removal, failure injection. | Recovery deleted journals/partials but did not undo already committed file moves. |
| Updater | Implemented for a signed-by-hash ZIP swap with backup rollback. | No tests. | ZIP traversal/reparse tests, interrupted swap recovery, updater process handoff. | The updater package hash is verified, but package signing/trusted publisher policy is not yet part of the code path. |
| Avalonia app | UI shell and view navigation scaffolding. | One ViewModel smoke test. | Headless startup/navigation binding-log validation and live API/library population. | Most pages are presentational; wiring them to a real launcher service graph remains future integration work. |
| CI/deployment | CI, Compose, Caddy, and documentation are present. | Build jobs only. | Cross-language signing, local E2E, corruption/recovery suites, optional PostgreSQL service job. | Docker/PostgreSQL availability differs between developer machines and CI. |

## Changes made in this validation phase

This phase adds the missing integrity and recovery boundaries rather than hiding them behind mocks:

- signed-manifest envelopes with key IDs and cross-language verification;
- adversarial manifest/path, storage, downloader, installer, updater, and SQLite tests;
- deterministic Synthetic Game A/B generation and a real local API-to-launcher runner;
- resumable downloads with per-chunk metrics and durable download jobs;
- update/repair transactions with explicit rollback and recovery journals;
- local publication/catalog ingestion rather than an unqualified in-memory response;
- FastCDC reuse measurements, failure-injection coverage, and performance baselines.

The infrastructure follow-up now includes the S3-compatible provider, multipart/retry/hash verification, multi-location resolution, provider health, and operator publication wiring. Live S3 credentials, DNS/TLS, VPS deployment, and public traffic remain deliberately out of scope for this repository-only run. The S3 suite uses an in-process S3-compatible HTTP fixture; it does not prove behavior of a particular cloud vendor. A PostgreSQL test is only counted as passing when a real disposable PostgreSQL process/container is available; otherwise the report records the environmental blocker.

## Current validation status (2026-08-14)

The table above is the historical baseline from `da2aeea`; it is not a description of the current tree. The follow-up validation now has the following evidence:

- Manifest signatures, trusted-key verification, adversarial path checks, storage/download/installer/updater tests, and cross-language CI are implemented. Production signing still requires an externally managed private key; the production gate fails closed when that key is absent.
- The Rust ingest worker accepts directories plus ZIP, RAR, 7z, TAR, `.tar.gz`, and `.tar.bz2`, extracts with bounded temporary-space and traversal/link protections, then runs analysis and physical-pack packaging.
- PostgreSQL-backed publication, physical-pack resolution, FileMirage HOT placement, Telegram COLD placement, COLD-to-HOT pack streaming, repair, and restart checks have been exercised against Mantle staging. The verified real fixture was the authorized Steam Spacewar installation; no unauthorized game content is included in the repository.
- The Avalonia runtime now hydrates the catalog from the API, derives install/update state from persisted local state, and wires library/search/sidebar, download jobs, pause/resume, install/update/repair/uninstall/play, and settings navigation through the runtime. The client now follows all catalog pages. Remaining UI work is interactive visual QA and release packaging, not the original service-graph wiring.
- GitHub CI is green for website, .NET, Rust, analyzer, and E2E jobs. Mantle `/v1/health`, `/v1/ready`, authenticated metrics, and a local PostgreSQL dump/checksum have passed. Off-host backup replication is not yet configured.
- Mantle is currently HTTP-only because `vaultnode.pp.ua` has no published A/AAAA record. HTTPS/Caddy templates are present but ACME/public launcher validation cannot be claimed until DNS points at the VPS. Buzzheavier remains disabled until direct download/range/delete behavior is proven; GoFile is not implemented.
