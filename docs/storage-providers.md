# Storage providers and pools

The server uses `StorageRegistry` to expose one or more `StorageProvider`
implementations behind logical `StoragePool` metadata. Provider operations
(upload, read, delete, capacity, health) stay separate from pool placement and
capacity provisioning. A class may have multiple pools, and a pool may contain
multiple capacity members.

The built-in providers are:

- `local`: content-addressed files under `LAUNCHER_STORAGE_ROOT/chunks/encoded`, exposed through `/objects/{hash}`.
- `s3`: an S3-compatible endpoint configured by `LAUNCHER_S3_*`. It uses bounded retries, in-flight operations, and multipart uploads.
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
