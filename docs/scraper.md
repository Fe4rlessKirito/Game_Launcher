# Server-side release scraper

The optional `scraper/` package discovers releases from operator-authorized
HTTP(S) sources and stops at a validated artifact plus a normalized
`handoff.json`. It does not modify the launcher client and it does not publish
anything. The existing Rust normalizer and packager remain the only path from
an extracted artifact to launcher content.

```text
source registry / scheduler
        -> known deterministic adapter
        -> bounded semantic page + optional Playwright/CloakBrowser session
        -> Gemini action planner only when the adapter cannot continue
        -> redirect-checked downloader with resume and budgets
        -> magic/archive/checksum/duplicate validation
        -> handoff.json -> launcher-admin ingest -> analyzer -> packager
```

## Safety boundary

Sources are explicit records with a base URL, adapter name, platform/language
filters, request politeness, retry eligibility, and a per-source Gemini
fallback flag. The generic adapter is intentionally conservative. A site
specific adapter implements `SourceAdapter` and can be registered without
changing the browser, planner, downloader, or validator.

The browser reducer sends Gemini a bounded `PageSnapshot`, not raw HTML. It
contains visible text, headings, metadata, links, buttons, forms, pagination,
download markers, stable target IDs, and a state hash. Gemini can return only
the native structured action enum:

`FOLLOW_LINK`, `CLICK`, `SCROLL`, `WAIT`, `GO_BACK`, `EXTRACT_RELEASE`,
`REQUEST_MORE_CONTEXT`, or `ABORT`.

The action parser rejects unknown fields, invented selectors, missing targets,
invalid numeric ranges, and non-integer scroll values. The planner validates
target IDs against the current snapshot and stops on low confidence,
anti-bot/challenge text, repeated semantic states, navigation depth, page,
action, Gemini-call, or runtime budgets. Gemini is never used for trust
decisions, arbitrary code, shell commands, or unrestricted URL selection.

The default HTTP and Playwright paths enforce the same URL policy. Embedded
credentials, non-HTTP schemes, localhost/private/link-local/reserved/metadata
addresses, blocked DNS resolutions, and unsafe redirects are rejected. The
`--allow-localhost` option exists only for local fixture tests and must not be
enabled in deployment.

## Browser backends

`HttpBrowserExecutor` is sufficient for conventional release pages and is the
default. `PlaywrightBrowserExecutor` is the JavaScript-capable backend for
sites that need rendering or clicks. A CloakBrowser-compatible executable can
be selected with `SCRAPER_BROWSER_EXECUTABLE`; the abstraction does not depend
on a vendor-specific API. Playwright is optional:

```powershell
pip install -e scraper[dev,browser]
```

The generic HTTP path cannot execute JavaScript clicks. Such a source should
use the Playwright/CloakBrowser backend or be given a deterministic adapter.
Anti-bot challenges are reported as `CHALLENGE_REQUIRED` for operator review;
the scraper does not attempt to bypass them. Direct acquisition from an
in-session ingest receives only cookies matching the candidate URL, and those
cookies are stripped before a cross-host redirect.

## Acquisition and validation

Downloads are streamed into hidden `.part` files, support HTTP range resume,
follow only a bounded number of policy-checked redirects, enforce response
size limits, and verify an optional reported SHA-256/BLAKE3 checksum before an
atomic rename. Per-domain spacing and concurrency are applied for the whole
response stream, not only connection setup. Higher-ranked candidates are tried
only when a candidate fails a non-transient download or validation check.

The validator rejects HTML masquerading as an artifact, bad status, empty or
oversized files, extension/magic mismatches, bad ZIP framing/CRC, traversal,
duplicate paths, symbolic links, hard links, devices, FIFOs, and bounded TAR
expansion. ZIP/TAR limits are inspected before downstream processing. RAR and
7z framing is recognized; the existing Rust normalizer performs their final
bounded member validation before packaging.

Every successful run writes a handoff containing the source/release metadata,
the selected download and redirect chain, BLAKE3/SHA-256, archive facts,
warnings, and the exact downstream command shape:

```text
launcher-admin ingest <artifact> --output <package-dir>
```

The handoff is an operator boundary, not an automatic publish operation.

## Durable jobs and observability

`SQLiteJobStore` uses WAL mode, active-job deduplication, leases, retries, and
restart recovery. `IngestionScheduler` advances due sources, persists
discovery state before acquisition, records visited URLs/action history/Gemini
and browser counts, and classifies terminal failures for `inspect-job` and
`list-failures`. This standalone state store is deliberately separate from the
Rust/PostgreSQL packaging jobs; a PostgreSQL implementation can replace it
behind the same small store contract later.

No raw DOM is retained by default. Structured logs include source, domain,
adapter, URL, semantic state hash, result status, byte count, and artifact
hashes. Set `SCRAPER_DIAGNOSTICS_ENABLED=true` for bounded, HTML-free JSON
diagnostics under `SCRAPER_DIAGNOSTICS_DIR`; these contain semantic snapshots
and action history, never raw DOM. Keep sensitive browser profiles and API keys
outside the repository.

## CLI

Install the isolated package from the repository root:

```powershell
pip install -e scraper[dev]
```

Global options go before the subcommand. A source is registered once, then can
be discovered directly, ingested directly, or scheduled:

```powershell
launcher-scraper --store .\scraper-state.db source-add example https://downloads.example.invalid/releases
launcher-scraper --store .\scraper-state.db source-list
launcher-scraper --store .\scraper-state.db discover example
launcher-scraper --store .\scraper-state.db ingest example --output .\scraper-artifacts
launcher-scraper --store .\scraper-state.db --output-root .\scraper-artifacts worker --once
launcher-scraper --store .\scraper-state.db list-failures
launcher-scraper --store .\scraper-state.db inspect-job <job-id>
launcher-scraper adapter-list
```

The worker refuses to start unless `SCRAPER_ENABLED=true` is present in its
environment. This guard is independent of the per-source `enabled` flag.

For a local fixture only:

```powershell
launcher-scraper --allow-localhost --store .\fixture.db discover fixture
```

The Gemini fallback requires `GEMINI_API_KEY`. Set
`SCRAPER_GEMINI_MODEL` to a model available to the account, or leave it empty
to discover a model advertising `generateContent` at runtime. The source
record still controls whether fallback is allowed.

## Configuration

The most important deployment variables are documented in
`deploy/env.example`:

| Variable | Purpose |
| --- | --- |
| `SCRAPER_ENABLED` | deployment-level feature switch; keep false until sources and operator authorization are configured |
| `SCRAPER_STATE_DB` / `SCRAPER_OUTPUT_DIR` | persistent job state and validated artifact roots |
| `SCRAPER_BROWSER` | `http` or optional `playwright` |
| `SCRAPER_MAX_PAGES`, `SCRAPER_MAX_ACTIONS`, `SCRAPER_MAX_GEMINI_CALLS` | planner budgets |
| `SCRAPER_JOB_TIMEOUT_SECONDS` | total unknown-site planning budget |
| `SCRAPER_MAX_ARTIFACT_BYTES`, `SCRAPER_MAX_ARCHIVE_*` | download and archive bounds |
| `SCRAPER_TEMP_MAX_BYTES` | aggregate scratch reservation |
| `SCRAPER_DIAGNOSTICS_*` | optional bounded JSON diagnostics for worker failures |
| `GEMINI_API_KEY` / `SCRAPER_GEMINI_MODEL` | optional structured fallback credentials; never commit values |

`SCRAPER_ENABLED` is a deployment guard for the process entrypoint; the Python
library itself remains explicit and does not silently scrape sources. Source
authorization and distribution rights remain operator responsibilities.

## Tests

The fixture suite uses only loopback HTTP servers and synthetic ZIP data:

```powershell
$env:PYTHONPATH = (Join-Path (Get-Location) 'scraper/src')
python -m pytest -q scraper/tests
python -m ruff check scraper/src scraper/tests
```

The tests cover deterministic ranking, SSRF policy, redirects and resume,
archive validation, planner bounds, durable leases, and the complete local
artifact-to-handoff path without a Gemini call.
