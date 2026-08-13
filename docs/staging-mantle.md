# Mantle staging topology and validation

Mantle is the active staging host for Vaultnode. The VPS runs the API,
PostgreSQL, worker, private Telegram Local Bot API, private Telegram file
proxy, and Caddy through the repository's Docker Compose files. Only Caddy is
public; PostgreSQL, the worker, and both Telegram services remain private.

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

## Operator checks

```text
curl -fsS https://<public-api-host>/v1/health
curl -fsS https://<public-api-host>/v1/ready
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T worker launcher-admin storage health
```

From the workstation, the Mantle-aware scripts use SSH and never require a
Railway CLI:

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

The Telegram smoke is intentionally tiny and proves Bot API reachability,
physical-pack upload/download, exact-byte and BLAKE3 verification, and
temporary-message deletion. It is not the 512 MiB performance gate.

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
