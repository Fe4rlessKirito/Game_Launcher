# Dependency baseline

The repository pins the major/minor compatibility baseline in the project manifests and records exact Rust resolution in `server/Cargo.lock`.

| Area | Baseline |
| --- | --- |
| Desktop | .NET 10.0.302 SDK, Avalonia 12.1.0, CommunityToolkit.Mvvm 8.4.0 |
| Client integrity | Blake3 3.0.2, ZstdSharp.Port 0.8.8 |
| Backend | Rust stable, Axum 0.8, SQLx 0.9, PostgreSQL |
| Packager | FastCDC 4.0.1, BLAKE3 1.8, Zstandard 0.13 |
| Analyzer | Python 3.12+, optional LIEF 0.17.6+ |
| Website | Astro 7.2, Tailwind CSS 4.3 through `@tailwindcss/vite` |

The Windows-first SQLite build uses `Microsoft.Data.Sqlite.Core` with the Windows system SQLite provider to avoid bundling the audited native `lib.e_sqlite3` package. A non-Windows provider must be selected explicitly before shipping a macOS/Linux artifact.

FastCDC 4.0.1 currently accepts maximum chunk sizes through 16 MiB in its streaming implementation. The packager therefore emits the truthful 1/4/16 MiB v1 profile and rejects out-of-range settings instead of silently encoding a different format.
