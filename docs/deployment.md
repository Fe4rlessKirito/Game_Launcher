# Deployment

The installed `vaultnode-postgres-backup.service` loads the deployment `.env`
before running `deploy/backup-postgres.sh`. This is required for
`BACKUP_REPLICATION_*` to reach the systemd-triggered job. An environment file
with missing replication values still produces only a local backup unless
`BACKUP_REPLICATION_REQUIRED=true`, in which case the job fails closed.

`deploy/compose.yaml` starts PostgreSQL, the API, and Caddy. Copy `deploy/env.example` into the staging secret-management workflow; do not commit a populated environment file. Run migrations explicitly before starting production traffic. PostgreSQL backups use `pg_dump --format=custom`. `deploy/backup-postgres.sh` verifies the local checksum and asks the PostgreSQL image's `pg_restore` to validate the custom-dump directory before reporting success or replicating the file. It can also replicate each verified dump and checksum to an explicitly configured SSH host using `BACKUP_REPLICATION_*`; the remote copy is checksum-checked before the job succeeds. The replication path uses batch mode and strict host-key checking; set `BACKUP_REPLICATION_REQUIRED=true` to fail the backup job closed when the off-host destination is not configured.

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

## Mantle deployment

Mantle is the active deployment target. The VPS compose override is the
authoritative production-shaped topology. Before switching the launcher from
the current temporary VPS-IP URL, point DNS at the VPS, set `SITE_HOST` and
`ACME_EMAIL`, and let Caddy obtain the HTTPS certificate. Do not expose the
operator token, database URL, Telegram credentials, or signing key to clients.

The override requires:

```text
LAUNCHER_PUBLIC_BASE_URL=https://<public-api-host>
SITE_HOST=<public-api-host>
ACME_EMAIL=<certificate-contact>
LAUNCHER_OPERATOR_TOKEN=<random-secret-at-least-32-bytes>
LAUNCHER_OPERATOR_AUTH_REQUIRED=true
LAUNCHER_SIGNING_REQUIRE_EXTERNAL_KEY=true
```

Caddy receives `SITE_HOST` and `ACME_EMAIL` from the Compose environment. The
checked-in HTTP Mantle file is a wildcarded plaintext fallback that redirects
to `https://vaultnode.pp.ua`; the HTTPS file uses the host variables for the
ACME certificate cutover. Recreate Caddy after changing either value and
validate the rendered configuration before opening public traffic.

When an operator token is configured, the API requires at least 32 bytes and
compares bearer values without an early-exit equality check. Keep the token in
the deployment secret store; never put it in launcher settings, logs, or a
client build.

With `LAUNCHER_SIGNING_REQUIRE_EXTERNAL_KEY=true`, an admin publish/signing
job fails closed when the secret-managed signing key is absent; it will not
generate a local fixture key. The private key must be supplied through the
deployment secret store or an external signing service.

The Mantle override mounts the operator-managed key at
`/run/secrets/mantle-signing-key.pem` for the worker only. Keep the host file
mode `600`, owned by the container's `launcher` UID, and never copy the
private key into the repository or a client settings file.

Production launcher settings must also contain the matching public key ring
and set `requireTrustedManifestKeys` to `true` (see
`deploy/launcher-production.example.json`). Development settings may leave
this disabled, which permits the embedded fixture key in a signature envelope.

The API also applies a 50 MiB request-body limit and a 256-request global
concurrency limit by default. Keep both limits enabled in production; adjust
`LAUNCHER_MAX_REQUEST_BYTES` and `LAUNCHER_MAX_CONCURRENT_REQUESTS` only when
the deployment has been measured and the values are set in the secret-managed
environment.

Provider health probes used by storage status and metrics are bounded by
`LAUNCHER_STORAGE_HEALTH_TIMEOUT_SECONDS` (15 seconds by default, capped at 120
seconds), so an unavailable HOT provider cannot hang operator monitoring.

The API also applies a bounded global request window. `LAUNCHER_RATE_LIMIT_REQUESTS`
defaults to 600 requests per `LAUNCHER_RATE_LIMIT_WINDOW_SECONDS` (default 60).
Exceeded requests receive HTTP 429 and a `Retry-After` header. Restore admission
has a separate 30-request window, a 16-job per-request cap, and a combined
logical/physical queue cap controlled by `LAUNCHER_MAX_PENDING_RESTORE_JOBS`.
Keep these limits appropriate for the number of launcher clients and retain the
concurrency and body-size limits as separate protections.

Before calling the VPS production-shaped, run the fail-closed public cutover
check from a host that can resolve the final DNS record:

```bash
export LAUNCHER_PUBLIC_BASE_URL=https://vaultnode.pp.ua
export SITE_HOST=vaultnode.pp.ua
export ACME_EMAIL=operator@example.invalid
export LAUNCHER_OPERATOR_TOKEN='read-from-the-secret-store'
export MANTLE_PUBLIC_IP=5.231.32.191
export BACKUP_REPLICATION_REQUIRED=true
export BACKUP_REPLICATION_HOST=backup.example.invalid
export BACKUP_REPLICATION_DIR=/srv/backups/vaultnode
export BACKUP_REPLICATION_IDENTITY_FILE=/root/.ssh/vaultnode-backup
bash scripts/mantle-production-check.sh
```

The command verifies the DNS target, certificate-validated HTTPS health and
readiness, authenticated metrics, plaintext redirect, and the required
off-host backup configuration. It deliberately cannot pass against the
temporary VPS-IP HTTP endpoint or with only a local PostgreSQL dump.

The launcher defaults to `https://vaultnode.pp.ua` and migrates the historical
Mantle IP/local URLs to that HTTPS endpoint. During DNS cutover, set
`LAUNCHER_API_BASE_URL` or the local settings file to the final HTTPS hostname;
do not ship an IP or HTTP API URL in a release build.
