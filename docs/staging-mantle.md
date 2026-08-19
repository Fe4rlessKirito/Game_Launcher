# Mantle staging topology and validation

Mantle is the active staging host for Vaultnode. The VPS runs the API,
PostgreSQL, Rust worker, release scraper, private Telegram Local Bot API,
private Telegram file proxy, and Caddy through the repository's Docker Compose files. Only Caddy is
public; PostgreSQL, the worker, and both Telegram services remain private.
The checked-in `deploy/VpsCaddyfile` remains the HTTP fallback for local or
IP-only staging. The active Mantle deployment now uses
`deploy/VpsCaddyfile.https.example` with `SITE_HOST=vaultnode.pp.ua` and a
Let's Encrypt certificate. Caddy receives `SITE_HOST` and `ACME_EMAIL` through
the Compose environment so the HTTPS configuration is reproducible. When
recreating the deployment, keep the HTTPS file mounted and verify the public
certificate before enabling client traffic.
The checked-in `deploy/vps.compose.https.yaml` is the reproducible switch for
that cutover; it mounts the HTTPS Caddyfile without editing the base compose
files. The health-check script rejects HTTP by default after the cutover.
The production-shaped override requires `LAUNCHER_OPERATOR_TOKEN` for storage
diagnostics and Prometheus metrics, and sets
`LAUNCHER_SIGNING_REQUIRE_EXTERNAL_KEY=true` in the API/worker environment.
The same token is injected into the private worker so its `staging verify`
command can authenticate those protected diagnostics without exposing the
token to launcher clients.
Keep the operator token and the external manifest-signing key in VPS secret
storage; neither is included in the launcher or returned by the API.

## Deploy the stack

On the VPS, from `/home/debian/vaultnode`:

```text
git pull --ff-only
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml up -d --build
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml ps
```

Create `secrets/telegram.env` and `secrets/telegram-api.env` directly on the
VPS with mode `600`. Keep bot credentials, Telegram API credentials, database
credentials, and signing secrets out of Git, chat, and logs. The first file is
read by the API and worker; the second is read only by the local Telegram Bot
API service and contains `TELEGRAM_API_ID` and `TELEGRAM_API_HASH`.

The active override keeps FileMirage as HOT and Telegram as physical-pack
COLD. `LAUNCHER_PACK_COLD_ONLY=true` means Telegram receives physical packs,
not an additional logical-chunk copy. The Bot API service has no public port;
the worker uses `http://telegram-bot-api-proxy:8081` over the Compose network.
The release scraper is enabled on Mantle for explicitly authorized sources.
Its SQLite state and redacted work-status records stay on the persistent
control-plane volume, while downloaded artifacts use the shared transient
volume. The scraper still stops at a validated `handoff.json`; the operator
then runs the normalizer/packager and `launcher-admin publish` workflow.

`LAUNCHER_STORAGE_ROOT` points at the shared `launcher-ephemeral-content`
RAM-backed volume for API/worker staging, pack construction, and restore
scratch. The persistent `launcher-storage` volume is reserved for provider
indexes, Telegram/FileMirage state, work-status metadata, and other small
control-plane records. The transient volume is bounded to 8 GiB and is lost
when its tmpfs mount or the VPS is restarted; it is not a durable game
library. With `LAUNCHER_CLEANUP_STAGING_AFTER_PUBLISH=true`, the publish
command removes the package, copied staging chunks, and recorded scraper
artifact only after HOT/COLD publication succeeds. Failed publication keeps
the staging files available for retry. Mantle is never the canonical
game-byte store; the published physical packs live on FileMirage HOT and
Telegram COLD.

## Operator checks

```text
curl -fsS https://<public-api-host>/v1/health
curl -fsS https://<public-api-host>/v1/ready
curl -fsS -H "Authorization: Bearer $LAUNCHER_OPERATOR_TOKEN" https://<public-api-host>/metrics
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T worker launcher-admin storage health
```

For a temporary IP-only smoke endpoint, explicitly opt into the exception:

```text
LAUNCHER_ALLOW_HTTP_HEALTHCHECK=true \
LAUNCHER_PUBLIC_BASE_URL=http://<mantle-ip> \
LAUNCHER_OPERATOR_TOKEN=<secret> \
scripts/mantle-healthcheck.sh
```

Do not use that exception for a public release.

From the workstation, the Mantle-aware scripts use SSH:

```powershell
.\scripts\staging\telegram-smoke.ps1 -Mantle `
  -RemoteHost <mantle-vps-ip> `
  -RemoteUser debian `
  -IdentityFile C:\path\to\new_key `
  -RemoteDirectory /home/debian/vaultnode

.\scripts\staging\publish-synthetic.ps1 -Mantle `
  -RemoteHost <mantle-vps-ip> `
  -IdentityFile C:\path\to\new_key
```

When running `scripts/staging/verify-staging.ps1` against a deployment with
operator authentication enabled, keep `LAUNCHER_OPERATOR_TOKEN` in the local
secret environment as well. The verifier uses it only for the protected
storage-status and metrics requests; it never prints or persists the token.

The Telegram smoke is intentionally tiny and proves Bot API reachability,
physical-pack upload/download, exact-byte and BLAKE3 verification, and
temporary-message deletion. It is not the 512 MiB performance gate.

## Backups and monitoring

Install the checked-in `deploy/vaultnode-postgres-backup.service` and
`deploy/vaultnode-postgres-backup.timer` units on the VPS, or use an equivalent
managed scheduler, to run `deploy/backup-postgres.sh`. It creates a
custom-format PostgreSQL dump and checksum in the explicit `BACKUP_DIR`,
verifies the checksum and custom-dump structure before success, retains the
configured window, and never logs database contents. If off-host replication
is configured, the destination checksum is verified before the job succeeds.
The current
no-user staging deployment keeps these backups on the VPS with
`BACKUP_REPLICATION_REQUIRED=false`; this protects against application and
database mistakes but is not an off-host disaster-recovery copy.

For a production cutover, copy dumps off-host before the retention window
expires, then restore one into a disposable PostgreSQL instance. Configure the
`BACKUP_REPLICATION_*` variables for an SSH destination and set
`BACKUP_REPLICATION_REQUIRED=true` only once that destination is ready so a
failed replication prevents the backup job from reporting success.

Example installation:

```bash
sudo install -m 0644 deploy/vaultnode-postgres-backup.service deploy/vaultnode-postgres-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now vaultnode-postgres-backup.timer
systemctl list-timers vaultnode-postgres-backup.timer
```

Run `scripts/mantle-healthcheck.sh` from a monitoring job with
`LAUNCHER_PUBLIC_BASE_URL` and the operator token in its secret environment.
It checks liveness, database readiness, and the authenticated metrics surface.

After a real staging build has a recorded build ID and physical-pack hash, the
destructive recovery check is explicit:

```powershell
.\scripts\staging\cold-restore-test.ps1 `
  -BuildId staging-<id> `
  -EncodedHash <64-character-lowercase-blake3> `
  -Confirm -Mantle `
  -RemoteHost <mantle-vps-ip> `
  -IdentityFile C:\path\to\new_key
```

That check is the point at which HOT is deliberately removed and COLD is
restored through the worker. Do not call recovery validated until the restored
pack is BLAKE3-verified and available from HOT again.
