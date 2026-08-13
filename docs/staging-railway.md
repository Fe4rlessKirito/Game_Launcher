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
| Restore Worker | Private only; no domain | Telegram COLD health, pack restore queue, HOT renewal |
| Provisioning Worker | Private only; no domain | Durable capacity-job retries/expiry and operator wake-up |
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
    LAUNCHER_STORAGE_MIN_HOT_FAILURE_DOMAINS=1
    LAUNCHER_STORAGE_MIN_COLD_FAILURE_DOMAINS=1
    LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS=1
    LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS=1
    LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED=true
    LAUNCHER_STORAGE_RESTORE_MODE=PROACTIVE
    LAUNCHER_RESTORE_TARGET_PROVIDER=railway-hot
    LAUNCHER_AUTO_MIGRATE=0

Set the same PostgreSQL and HOT references on the worker, then add:

    LAUNCHER_STORAGE_PROVIDERS=s3,telegram
    TELEGRAM_COLD_ENABLED=true
    TELEGRAM_BOT_API_BASE_URL=http://<telegram-bot-api-private-host>:8080
    TELEGRAM_BOT_TOKEN=<Railway secret>
    TELEGRAM_COLD_CHAT_IDS=<numeric chat id>
    TELEGRAM_COLD_MAX_UPLOAD_BYTES=536870912
    TELEGRAM_COLD_STATE_FILE=/var/lib/launcher/telegram/telegram-cold-state.json

Set these on the API and the private provisioning worker. Put the HMAC value in
Railway's secret variable UI and Cloudflare's Worker secret UI; the blank line
below is intentional:

    PROVISIONING_ENABLED=true
    PROVISIONING_EMAIL_DOMAIN=vaultnode.pp.ua
    PROVISIONING_EMAIL_INGEST_HMAC_SECRET=
    PROVISIONING_EMAIL_MAX_BYTES=5242880
    PROVISIONING_EMAIL_ALLOWED_CLOCK_SKEW_SECONDS=300
    PROVISIONING_MAIL_ALIAS_TTL_SECONDS=3600
    PROVISIONING_DEFAULT_MODE=MANUAL
    PROVISIONING_CAPACITY_HEADROOM_BYTES=0
    PROVISIONING_SECRET_STORE_DIR=/var/lib/launcher/provisioning-secrets
    PROVISIONING_TEMP_DIR=/tmp/launcher-cold

For a second service from the same repository, select the existing
`railway.worker.toml` and `deploy/worker.Dockerfile`, keep it private, and set
the service start command to
`/usr/local/bin/worker-entrypoint provisioning worker`. Give it only a tiny
volume mounted at `/var/lib/launcher/telegram` plus a separate secret-store
directory if the chosen secret-store implementation needs one. Do not mount
the HOT bucket or chunks as a worker volume.

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
5. Create a small worker volume mounted at `/var/lib/launcher/telegram`. The
   image entrypoint initializes the runtime-owned directory before dropping to
   the non-root launcher user.
6. Configure the private Local Bot API service and verify its persistent state
   volume. No MEGA runtime or account is required for staging.

The worker volume is for Telegram message/index state, not game storage. HOT
objects live in the bucket. Restore transfers use `TMPDIR=/tmp/launcher-cold`
and remove each temporary pack after BLAKE3 verification.

## First live checks

Run from an operator workstation with the Railway environment selected:

    launcher-admin db status
    launcher-admin staging verify --api-url $env:LAUNCHER_STAGING_API_URL --require-cold
    launcher-admin staging verify --api-url $env:LAUNCHER_STAGING_API_URL --manifest-build-id synthetic-staging-a --trusted-public-key .\staging-public-key.pem --expected-key-id staging-2026-01 --require-cold

The verify command calls only liveness, readiness, redacted storage status,
metrics, and optional manifest/signature endpoints. It does not mutate storage,
enqueue restores, or print response bodies. HTTPS is mandatory unless
--allow-http is explicitly used for a local smoke test.

The first real Telegram smoke is a separate gated operation: upload a tiny
random physical pack through the private Local Bot API, download it, verify
BLAKE3, delete the test message, and record the result before enabling the
restore worker. Telegram is the required COLD staging provider; MEGA is an
optional future adapter.

## Security boundaries

The public attack surface is the API HTTPS domain. PostgreSQL, the worker,
Local Bot API, the worker volume, Telegram credentials, and bucket credentials
remain private. Local
launcher-admin commands are operator actions; do not expose them as an
unauthenticated API. Railway TLS is the only public TLS termination in this
topology, and clients must reject invalid certificates.

Use separate staging and production buckets, database environments, Telegram
state/credentials, and signing keys. The staging launcher trusts only the explicit
staging-2026-01 public key and staging HTTPS endpoint. Production must not
contain that key or endpoint override.
