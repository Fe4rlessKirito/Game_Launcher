# Storage providers and pools

The server uses `StorageRegistry` to expose one or more `StorageProvider`
implementations behind logical `StoragePool` metadata. Provider operations
(upload, read, delete, capacity, health) stay separate from pool placement and
capacity provisioning. A class may have multiple pools, and a pool may contain
multiple capacity members.

The built-in providers are:

- `local`: content-addressed files under `LAUNCHER_STORAGE_ROOT/chunks/encoded`, exposed through `/objects/{hash}`.
- `s3`: an S3-compatible endpoint configured by `LAUNCHER_S3_*`. It uses bounded retries, in-flight operations, and multipart uploads.
- `filemirage`: FileMirage's public chunked upload API with persisted remote references and direct download URLs. Its observed contract does not include deletion or stable URLs.
- `buzzheavier`: Buzzheavier's HTTP PUT upload API. It is upload-only by default until a server-side direct-download and cleanup probe passes.
- `mega`: an operator-managed COLD pool backed by MEGAcmd sessions and PostgreSQL reservations.

## Configuration

`LAUNCHER_STORAGE_PROVIDERS` is a comma-separated ordered list. `local,s3` is
useful for staging; `s3` avoids retaining a second local byte store.
`LAUNCHER_STORAGE_PROVIDER` remains accepted as a compatibility alias for a
single provider.

S3 settings:

| Variable | Meaning | Default |
| --- | --- | --- |
| `LAUNCHER_S3_ENDPOINT` | S3-compatible HTTPS endpoint | required |
| `LAUNCHER_S3_REGION` | signing region | required |
| `LAUNCHER_S3_BUCKET` | bucket name | required |
| `LAUNCHER_S3_ACCESS_KEY` / `LAUNCHER_S3_SECRET_KEY` | server-only credentials | required |
| `LAUNCHER_S3_PUBLIC_BASE_URL` | stable bucket/CDN URL prefix | empty; presign |
| `LAUNCHER_S3_PRESIGN_TTL_SECONDS` | presigned GET lifetime | `900` |
| `LAUNCHER_S3_MULTIPART_THRESHOLD_BYTES` | size at which multipart starts | `8388608` |
| `LAUNCHER_S3_MULTIPART_PART_BYTES` | multipart part size; minimum 5 MiB | `8388608` |
| `LAUNCHER_S3_ORPHAN_MAX_AGE_SECONDS` | minimum age for cleanup | `86400` |
| `LAUNCHER_S3_MAX_ATTEMPTS` | SDK request attempts | `4` |
| `LAUNCHER_S3_MAX_CONCURRENT_REQUESTS` | bounded provider operations | `4` |
| `LAUNCHER_S3_FORCE_PATH_STYLE` | use path-style addressing | `true` |

FileMirage settings:

| Variable | Meaning | Default |
| --- | --- | --- |
| `LAUNCHER_FILEMIRAGE_BASE_URL` | FileMirage API base URL | `https://filemirage.com` |
| `LAUNCHER_FILEMIRAGE_UPLOAD_SERVER_URL` | optional upload server override | server selected by `/api/servers` |
| `LAUNCHER_FILEMIRAGE_API_TOKEN` | optional server-only API token | empty |
| `LAUNCHER_FILEMIRAGE_STATE_FILE` | persisted hash-to-URL state | `${LAUNCHER_STORAGE_ROOT}/filemirage-state.json` |
| `LAUNCHER_FILEMIRAGE_UPLOAD_CHUNK_BYTES` | bounded upload chunk size | `103809024` (99 MiB) |
| `LAUNCHER_FILEMIRAGE_MAX_CONCURRENT_REQUESTS` | upload/download request bound | `4` |

Buzzheavier settings:

| Variable | Meaning | Default |
| --- | --- | --- |
| `LAUNCHER_BUZZHEAVIER_UPLOAD_BASE_URL` | anonymous or account PUT endpoint | `https://w.buzzheavier.com` |
| `LAUNCHER_BUZZHEAVIER_DOWNLOAD_BASE_URL` | public API/download host | `https://buzzheavier.com` |
| `LAUNCHER_BUZZHEAVIER_ACCOUNT_ID` | optional server-only account credential | empty |
| `LAUNCHER_BUZZHEAVIER_STATE_FILE` | persisted hash-to-file-ID state | `${LAUNCHER_STORAGE_ROOT}/buzzheavier-state.json` |
| `LAUNCHER_BUZZHEAVIER_MAX_CONCURRENT_REQUESTS` | upload/download request bound | `2` |
| `LAUNCHER_BUZZHEAVIER_DIRECT_DOWNLOAD_PROVEN` | enable resolver URLs only after a real probe | `false` |
| `LAUNCHER_BUZZHEAVIER_RANGE_REQUESTS_PROVEN` | enable range capability only after a real probe | `false` |
| `LAUNCHER_BUZZHEAVIER_DELETE_PROVEN` | enable deletion only after an authenticated probe | `false` |

## Pool metadata

Every pool records:

| Field | Meaning |
| --- | --- |
| `id` | Stable placement/restore identity. |
| `storage_class` | `HOT`, `COLD`, or future `ARCHIVE`. |
| `provider_type` | `s3`, `mega`, `local`, or another implementation type. |
| `priority` | Lower numbers are preferred for placement and restore. |
| `failure_domain` | Shared outage boundary; all accounts in one MEGA pool use `mega`. |
| `enabled` / `status` | Operator placement gate and aggregate health (`READY`, `DEGRADED`, `NEEDS_CAPACITY`, `UNAVAILABLE`, `DISABLED`). |
| `provisioning_mode` | `DISABLED`, `MANUAL`, or `AUTOMATIC`. |

The current `mega-cold` pool is manual. The current Railway HOT pool is an S3
pool. Provider order is never used as a substitute for class or pool policy.
See [storage classes and policy](storage-tiers.md) for replica requirements,
publication gating, and restore behavior.

## Upload and verification behavior

The object key is deterministic: `chunks/encoded/{lowercase-blake3}.bin`.
Providers verify size and BLAKE3 after writes and never accept a mismatched
object as successfully published. MEGA uploads use the account pool's
reservation ledger and bounded temporary files. The API never exposes COLD
locations.

Use `launcher-admin storage health`, `launcher-admin storage pools list`, and
`/api/v1/storage/status` to inspect provider, pool, account, and reservation
status. These surfaces omit passwords and session material.

## Physical packs and capabilities

When `PACK_STORAGE_ENABLED=true`, providers also store immutable objects under
`packs/{pack-blake3}.pack`. Pack locations are tracked separately from logical
chunk locations. `/api/v1/storage/providers` reports whether a provider can
serve direct HOT pack downloads, accept ranges, refresh URLs, and the
recommended object size/concurrency. `launcher-admin storage probe` is the
operator-facing capability check.

The `telegram` provider is COLD-only and server-side. It uses the official
[Telegram Bot API](https://core.telegram.org/bots/api); its message/file state
is private to the restore worker. The 512 MiB staging target additionally
requires the official [Local Bot API Server](https://github.com/tdlib/telegram-bot-api)
in private `--local` mode. Buzzheavier's documented HTTP API is at
[Buzzheavier Developers](https://buzzheavier.com/developers), and GoFile's
documented API is at [GoFile API](https://gofile.io/api). FileMirage and
Buzzheavier can now be selected in `LAUNCHER_STORAGE_PROVIDERS`; capability
flags still gate client-facing behavior, so enabling Buzzheavier does not make
it a download source until direct download is explicitly proven.
