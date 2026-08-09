# Storage providers

The server uses `StorageRegistry` to expose one or more `StorageProvider` implementations. The operator placement engine selects the providers required by the configured tier policy. The built-in providers are:

- `local`: content-addressed files under `LAUNCHER_STORAGE_ROOT/chunks/encoded`, exposed through `/objects/{hash}`.
- `s3`: an S3-compatible endpoint configured by `LAUNCHER_S3_*` variables. It uses the AWS SDK's retry policy, bounded in-flight operations, single PUTs below the multipart threshold, and multipart uploads above it.

## Configuration

`LAUNCHER_STORAGE_PROVIDERS` is a comma-separated ordered list. `local,s3` is useful for staging; `s3` avoids retaining a second local byte store. `LAUNCHER_STORAGE_PROVIDER` remains accepted as a compatibility alias for a single provider.

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

The S3 identity needs bucket/object permissions for `HeadBucket`, `HeadObject`, `GetObject`, `PutObject`, `DeleteObject`, multipart create/upload/complete/abort, and multipart listing for cleanup. Keep the policy restricted to `chunks/encoded/*` where the provider supports prefix conditions. The health endpoint performs `HeadBucket`, so the identity must be allowed to discover bucket availability.

## Upload and verification behavior

The object key is deterministic: `chunks/encoded/{lowercase-blake3}.bin`. Before uploading, the provider performs `HeadObject`; matching size plus `x-amz-meta-blake3` is treated as an idempotent hit. If metadata is absent, it downloads and hashes the existing object before deciding. After a new upload it checks size/metadata and downloads the object once more for BLAKE3 verification. A mismatched object is never accepted as a successful publication.

Multipart failures issue an abort request and return the original failure. A scheduled cleanup removes only uploads older than `LAUNCHER_S3_ORPHAN_MAX_AGE_SECONDS`; providers should also have a bucket lifecycle rule for incomplete multipart uploads as a last-resort control. Uploads are retried by the SDK and each provider instance bounds concurrent operations with a semaphore.

`StorageRegistry` continues resolving healthy locations when one provider cannot create a download URL. If no provider or static mirror remains, the API returns `503 no_chunk_locations`. `/health` reports each provider's active health state and returns `degraded` when any configured provider fails its check.

## Tiered storage

Every provider declares `HOT` or `COLD`; provider order is not a substitute for
tier policy. See [storage tiers](storage-tiers.md) for replica requirements,
publication gating, and restore behavior. The `mega` provider is a cold pool
backed by isolated operator-managed MEGAcmd sessions. It is configured through
`LAUNCHER_MEGA_ACCOUNTS_FILE`, never returns client-facing locations, and uses
PostgreSQL capacity reservations before upload. See
[MEGA cold storage](mega-cold-storage.md) and
[capacity operations](storage-capacity.md).

Use `launcher-admin storage health` and `/api/v1/storage/status` to inspect
provider, pool, account, and reservation-facing status. These surfaces omit
passwords and session material. The `storage` admin commands do not accept a
password argument; enrollment references an already provisioned session.

## Publication and database records

`launcher-admin publish` uploads every manifest-referenced object to the providers selected by the configured placement plan. When `DATABASE_URL` is present it also creates/updates the build records, records verified storage objects and tiers, records only non-expiring direct URLs, and transitions the build to `PUBLISHED` only after all chunks satisfy the hot/cold replica policy. Presigned URLs are deliberately not stored because they expire; the API regenerates them on each resolution request.

Use separate buckets or prefixes for development, staging, and production. Do not share write credentials between environments.
