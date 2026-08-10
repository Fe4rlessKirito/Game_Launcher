# Railway staging topology and deployment

This is the deployment runbook for the first live staging environment. It is
non-secret: values below are variable references or placeholders, never
resolved credentials. The repository is not connected to a Railway project, so
this runbook does not claim that staging is deployed.

## Service topology

| Service | Exposure | Responsibilities |
| --- | --- | --- |
| api | One public HTTPS domain | Catalog, hot resolution, presigned URLs, restore-pending responses |
| Postgres | Private only | Catalog, storage locations, account ledger, reservations, restore jobs |
| Restore Worker | Private only; no domain | Cold health/capacity, restore queue, MEGAcmd transfers |
| HotBucket | Private S3 API | HOT object bytes through the generic S3 interface |
| website | Optional public HTTPS | Static shell; calls the public API |

Keep API, PostgreSQL, worker, and bucket in the same Railway project and
environment. PostgreSQL and the worker do not get public domains. No Caddy
service is needed on Railway.

## Config-as-code

The API config is railway.toml. The worker config is railway.worker.toml; select
it as the worker service's custom config-as-code file in Railway service
settings. Both use the current build/deploy keys. The API healthcheck is
/v1/health, which only proves that the process responds. Database readiness is
the separate /v1/ready check and is intentionally not used as the deployment
healthcheck.

The API uses Railway's injected PORT when LAUNCHER_BIND is absent and binds
0.0.0.0:$PORT. Do not hardcode a Railway hostname in source, config, or the
launcher.

## Variable wiring

Set these on the API service. Replace Postgres and HotBucket with the actual
Railway service names:

    DATABASE_URL=${{Postgres.DATABASE_URL}}
    LAUNCHER_PUBLIC_BASE_URL=https://${{RAILWAY_PUBLIC_DOMAIN}}
    LAUNCHER_STORAGE_PROVIDERS=s3
    LAUNCHER_S3_PROVIDER_ID=railway-hot
    LAUNCHER_S3_TIER=HOT
    LAUNCHER_S3_ENDPOINT=${{HotBucket.ENDPOINT}}
    LAUNCHER_S3_REGION=${{HotBucket.REGION}}
    LAUNCHER_S3_BUCKET=${{HotBucket.BUCKET}}
    LAUNCHER_S3_ACCESS_KEY=${{HotBucket.ACCESS_KEY_ID}}
    LAUNCHER_S3_SECRET_KEY=${{HotBucket.SECRET_ACCESS_KEY}}
    LAUNCHER_S3_SESSION_TOKEN=
    LAUNCHER_S3_PUBLIC_BASE_URL=
    LAUNCHER_S3_FORCE_PATH_STYLE=false
    LAUNCHER_S3_PRESIGN_TTL_SECONDS=900
    LAUNCHER_STORAGE_MIN_HOT_REPLICAS=1
    LAUNCHER_STORAGE_MIN_COLD_REPLICAS=1
    LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS=1
    LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS=1
    LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED=true
    LAUNCHER_STORAGE_RESTORE_MODE=PROACTIVE
    LAUNCHER_RESTORE_TARGET_PROVIDER=railway-hot
    LAUNCHER_AUTO_MIGRATE=0

Set the same PostgreSQL and HOT references on the worker, then add:

    LAUNCHER_STORAGE_PROVIDERS=s3,mega
    LAUNCHER_MEGA_ACCOUNTS_FILE=/var/lib/launcher/megacmd/mega-accounts.json

Railway Bucket exposes ENDPOINT, REGION, BUCKET, ACCESS_KEY_ID, and
SECRET_ACCESS_KEY reference variables. Those names are translated into the
launcher's generic LAUNCHER_S3_* names. Do not put resolved values in
deploy/env.example, Git, launcher settings, or client binaries.

## Database and migration sequence

1. Create a dedicated Railway staging environment, PostgreSQL service, private
   bucket, API service, and private worker service.
2. Set the variables above and deploy the API with LAUNCHER_AUTO_MIGRATE=1 for
   one controlled first boot, or run the migration from a controlled operator
   job.
3. Run launcher-admin db status with the staging DATABASE_URL. It reports
   CONNECTED, required tables, and schema_ready; it never prints the URL.
4. Set LAUNCHER_AUTO_MIGRATE=0, redeploy, and verify /v1/ready.
5. Create a small worker volume mounted at
   /var/lib/launcher/megacmd. The image entrypoint initializes the
   runtime-owned directory before dropping to the non-root launcher user.
6. Install a pinned official MEGAcmd package or operator-built image layer.
   Copy exactly one operator-managed account file to the volume, with a
   pre-authenticated session home for that same account. Never automate
   signup, password entry, CAPTCHA, or recovery.

The worker volume is for MEGAcmd session state and the account reference file,
not game storage. HOT objects live in the bucket. Restore transfers use
TMPDIR=/tmp/launcher-mega and remove each temporary chunk after BLAKE3
verification.

## First live checks

Run from an operator workstation with the Railway environment selected:

    launcher-admin db status
    launcher-admin staging verify --api-url $env:LAUNCHER_STAGING_API_URL --require-cold
    launcher-admin staging verify --api-url $env:LAUNCHER_STAGING_API_URL --manifest-build-id synthetic-staging-a --trusted-public-key .\staging-public-key.pem --expected-key-id staging-2026-01 --require-cold

The verify command calls only liveness, readiness, redacted storage status,
metrics, and optional manifest/signature endpoints. It does not mutate storage,
enqueue restores, or print response bodies. HTTPS is mandatory unless
--allow-http is explicitly used for a local smoke test.

The first real MEGA smoke is a separate gated operation: run health,
upload/size verification, download/BLAKE3 verification, and delete against one
synthetic chunk. Record a session-reuse restart test before enabling the
restore worker. A failed outbound call must be classified as
MEGA_NETWORK_UNAVAILABLE; an authenticated but rejected session is
MEGA_AUTH_FAILED. Do not retry authentication by logging a password.

## Security boundaries

The public attack surface is the API HTTPS domain. PostgreSQL, the worker,
MEGAcmd, the worker volume, and bucket credentials remain private. Local
launcher-admin commands are operator actions; do not expose them as an
unauthenticated API. Railway TLS is the only public TLS termination in this
topology, and clients must reject invalid certificates.

Use separate staging and production buckets, database environments, MEGA
sessions, and signing keys. The staging launcher trusts only the explicit
staging-2026-01 public key and staging HTTPS endpoint. Production must not
contain that key or endpoint override.
