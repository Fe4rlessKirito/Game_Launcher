# Deployment

`deploy/compose.yaml` starts PostgreSQL, the API, and Caddy. Copy `deploy/env.example` into the staging secret-management workflow; do not commit a populated environment file. Run migrations explicitly before starting production traffic. PostgreSQL backups should use `pg_dump --format=custom` and be restored into a disposable database before a release is trusted.

## Environments

Development defaults to `LAUNCHER_STORAGE_PROVIDERS=local` and serves objects through the API's local proxy. Staging should use the same API image with PostgreSQL, HTTPS through Caddy, and at least one independently operated object location:

```text
launcher-admin publish
        |
        v
  PostgreSQL <- Axum API <- Caddy/TLS <- staging launcher
        |
        +--> local mirror (optional)
        +--> S3-compatible bucket
```

Set `LAUNCHER_STORAGE_PROVIDERS=local,s3` to exercise two providers in one staging publish, or `s3` when the staging bucket is the only byte store. `LAUNCHER_MIRROR_BASE_URLS` adds externally operated `/objects/{hash}` mirrors to resolver output. The API returns provider URLs first, then verified database locations, then configured static mirrors, with duplicates removed.

For S3-compatible storage, set the endpoint, region, bucket, access key, and secret key. Set `LAUNCHER_S3_PUBLIC_BASE_URL` only when the bucket or CDN exposes stable object URLs; otherwise the API generates short-lived presigned GET URLs. Keep the bucket private when using presigning. The API never proxies S3 bytes.

The publisher uploads content-addressed objects under `chunks/encoded/{blake3}.bin`, verifies the returned object by size, metadata, and a downloaded BLAKE3 hash, and records only stable URLs in `storage_locations`. Presigned locations are resolved at request time. Schedule `S3CompatibleStorage::cleanup_orphaned_multipart_uploads` from an operator/maintenance job with credentials scoped to the staging bucket; the configured age threshold prevents fresh uploads from being aborted.

## Staging checklist

1. Provision a dedicated bucket and database; grant the publisher/API only the required bucket and object-prefix permissions.
2. Inject secrets through the VPS secret store or deployment manager. Never put S3 or signing secrets in Compose, Git, launcher configuration, or client binaries.
3. Configure a real DNS name, Caddy email, and `LAUNCHER_PUBLIC_BASE_URL=https://...`; verify `/health` reports every configured provider as healthy.
4. Run the migration, publish an authorized test build with `DATABASE_URL` and the provider configuration, and verify the catalog, manifest signature, presigned/stable URLs, and direct chunk downloads.
5. Run the A→B→repair workflow over HTTPS with an interrupted download and an unavailable mirror. Check that the launcher retries another URL and that no API request carries chunk bytes.
6. Exercise VPS restart, PostgreSQL restore, bucket lifecycle cleanup, key rotation, and rollback before calling the environment production-ready.

The repository does not contain a VPS hostname, DNS zone, bucket, or production credentials, so staging deployment and real HTTPS validation remain operator actions.

## Railway API deployment

The root `railway.toml` uses the current Railway config-as-code keys:

```toml
[build]
dockerfilePath = "deploy/api.Dockerfile"

[deploy]
startCommand = "/usr/local/bin/launcher-api"
healthcheckPath = "/v1/health"
healthcheckTimeout = 30
restartPolicyType = "ON_FAILURE"
restartPolicyMaxRetries = 10
```

`/v1/health` is process-only and does not scan storage or require PostgreSQL.
`/v1/ready` performs a cheap `SELECT 1` when `DATABASE_URL` is configured.
The API binds to `LAUNCHER_BIND` when explicitly set, otherwise to Railway's
`0.0.0.0:$PORT`; local development still defaults to
`127.0.0.1:8080`.

Create a Railway PostgreSQL service and set
`DATABASE_URL=${{Postgres.DATABASE_URL}}` on the API and worker. Run
`launcher-admin db status` against the service environment before enabling
traffic; use `LAUNCHER_AUTO_MIGRATE=1` only for a controlled first boot, then
return it to `0`. The status command is read-only and reports only connectivity
and required table presence.

The intended staging topology is:

```text
                         public HTTPS / Railway TLS
 staging launcher ────────────────────────> API service
                                               │
                                               ├── private PostgreSQL
                                               └── private Railway Bucket (S3 HOT)

 private Restore Worker ── PostgreSQL + Railway Bucket HOT + Telegram COLD
 website service (optional) ── public API URL only
```

The API service uses `LAUNCHER_STORAGE_PROVIDERS=s3`; the worker uses
`s3,telegram` and owns the private Telegram/Local Bot API connection. The API
never needs Telegram credentials to resolve hot locations. COLD pack state is
recorded in PostgreSQL by the operator/worker commands and is visible through
the redacted storage status endpoint. The worker has no HTTP endpoint and must
not receive a public domain.
Use `railway.worker.toml` as the service's custom config-as-code file; Railway
supports selecting a custom config path in service settings.

Create a small persistent volume on the worker and mount it at
`/var/lib/launcher/telegram`. Store only Telegram message/index state there,
not game objects. The worker image sets `TMPDIR=/tmp/launcher-cold`; the restore
path processes one bounded pack at a time and removes temporary files after
verification. The private Local Bot API service owns its own persistent state
volume. MEGA remains an optional adapter and is not required for staging.

Railway Bucket credentials are wired through generic S3 variables, for example:

```text
LAUNCHER_S3_ENDPOINT=${{HotBucket.ENDPOINT}}
LAUNCHER_S3_REGION=${{HotBucket.REGION}}
LAUNCHER_S3_BUCKET=${{HotBucket.BUCKET}}
LAUNCHER_S3_ACCESS_KEY=${{HotBucket.ACCESS_KEY_ID}}
LAUNCHER_S3_SECRET_KEY=${{HotBucket.SECRET_ACCESS_KEY}}
LAUNCHER_S3_FORCE_PATH_STYLE=false
```

Use the bucket's actual service name in the references. Keep the bucket
private and leave `LAUNCHER_S3_PUBLIC_BASE_URL` empty so the API returns
short-lived presigned GET URLs. The storage implementation remains generic
S3-compatible and does not branch on Railway.

Run the Astro website as a separate Railway service rooted at `website/`; the
API service does not serve the website bundle. Railway provides TLS and public
service domains, so Caddy is not part of the Railway topology. Keep the
existing Caddy Compose topology for a VPS deployment. See
`docs/staging-railway.md` for the non-secret variable matrix and deployment
sequence.
