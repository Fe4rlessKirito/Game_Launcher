# Launcher Platform

Launcher is a content-addressed game distribution platform for builds the operator is authorized to distribute and support. The repository is a deliberately separated monorepo: a native Avalonia client, a Rust control plane and packaging pipeline, a Python analyzer, and a small static Astro site.

The current implementation is an extensible v1 foundation with a complete synthetic local flow:

1. Analyze an authorized game directory with the Python CLI.
2. Normalize an authorized ZIP/RAR/7z/TAR input into a bounded temporary directory when necessary.
3. Package deterministic files into FastCDC/BLAKE3/Zstandard chunks with the Rust packager.
4. Serve catalog and manifest metadata from the Rust API.
5. Download, verify, reconstruct, install, repair, launch, and uninstall from the .NET client core.

No acquisition, DRM circumvention, license bypass, anti-cheat bypass, account spoofing, or unauthorized service access is implemented or supported.

## Repository map

| Area | Location | Responsibility |
| --- | --- | --- |
| Desktop client | `launcher/` | Avalonia UI, local state, download and installation engines |
| Control plane | `server/` | Axum API, typed domain model, PostgreSQL access, storage and packager |
| Analyzer | `analyzer/` | Deterministic executable/support-profile discovery |
| Website | `website/` | Static Astro/Tailwind public landing page |
| Contract | `schema/`, `migrations/`, `docs/` | Manifest, API, database, and operational specifications |
| Deployment | `deploy/` | Docker Compose PostgreSQL and Caddy configuration |

## Quick start

### Prerequisites

- .NET SDK 10.0.302 or later on the 10.0 feature band
- Rust stable
- Python 3.12+
- Node.js 22+
- Docker Desktop for PostgreSQL/API deployment checks (optional for local packager/analyzer work)

The included `global.json` pins the tested .NET SDK feature band. If you use the repository-local SDK installed by the build agent, run `dotnet` through `./.dotnet/dotnet` on PowerShell.

### Python analyzer

```powershell
python -m venv analyzer/.venv
analyzer/.venv/Scripts/pip install -e analyzer[dev]
python -m launcher_analyzer analyze .\path\to\authorized\build --output analysis.json --json
```

### Rust workspace

```powershell
cargo test --workspace
cargo run -p launcher-packager -- package .\path\to\authorized\build --output .\artifacts\sample-build
```

### .NET client

```powershell
dotnet restore launcher/Launcher.sln
dotnet test launcher/Launcher.sln
dotnet run --project launcher/src/Launcher.App
```

### Website

```powershell
cd website
npm ci
npm run build
```

## Development principles

- API requests resolve metadata and storage locations; the launcher downloads bytes directly from storage.
- Raw and encoded BLAKE3 hashes are both verified. Unverified content never reaches the installation root.
- Manifest paths are portable `/` paths and are rejected if they escape the installation root.
- Ingestion is staged (`DISCOVERED → ANALYZED → PACKAGED → UPLOADED → VERIFIED → READY → PUBLISHED`); analysis never publishes automatically.
- Local state is SQLite and remote catalog state is PostgreSQL. Neither is used as a hidden substitute for the other.
- Expensive work is asynchronous and cancellable; UI view models receive coalesced progress updates.

See [docs/architecture.md](docs/architecture.md), [docs/manifest-format.md](docs/manifest-format.md), and [docs/deployment.md](docs/deployment.md) for the detailed contracts and known limitations.
