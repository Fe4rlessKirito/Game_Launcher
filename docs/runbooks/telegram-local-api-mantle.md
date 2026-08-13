# Private Telegram Local Bot API on Mantle

Mantle is the active staging host for Vaultnode. The local Telegram Bot API
and its file proxy run as private Docker Compose services on the VPS. Neither
service gets a public port or domain. The worker reaches the proxy only on the
Compose network.

## Services

Use the base stack plus the Mantle override from the repository root:

```text
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml up -d --build
```

The override starts:

- `telegram-bot-api`, built from the pinned official Telegram Bot API source;
- `telegram-bot-api-proxy`, which forwards Bot API calls and safely serves only
  local file paths returned by `getFile`;
- `worker`, with Telegram as the physical-pack COLD provider.

The source build belongs on the VPS because it is a substantial C++ build. The
Bot API state volume at `/var/lib/telegram-bot-api` is only for Telegram's
state and local file handling; launcher chunks and packs use the separate
`launcher-storage` volume and bounded `/tmp/launcher-cold` space.

Create these files on the VPS with mode `600`; never commit or paste them:

```text
secrets/telegram.env
secrets/telegram-api.env
```

`telegram.env` supplies the bot token, COLD chat ID, database/storage
credentials, and the launcher Telegram settings. `telegram-api.env` supplies
only `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` to the local Bot API container.
The service wiring uses:

```text
TELEGRAM_BOT_API_BASE_URL=http://telegram-bot-api-proxy:8081
TELEGRAM_COLD_ENABLED=true
TELEGRAM_COLD_MAX_UPLOAD_BYTES=536870912
TELEGRAM_COLD_STATE_FILE=/var/lib/launcher/storage/telegram-cold-state.json
LAUNCHER_STORAGE_PROVIDERS=filemirage,telegram
LAUNCHER_PACK_COLD_ONLY=true
```

The override keeps FileMirage as HOT and Telegram as physical-pack COLD. It
does not publish Telegram URLs to launcher clients.

## Tiny live smoke

From the operator workstation, run the smoke through the private worker:

```powershell
.\scripts\staging\telegram-smoke.ps1 -Mantle `
  -RemoteHost <mantle-vps-ip> `
  -RemoteUser debian `
  -IdentityFile C:\path\to\new_key `
  -RemoteDirectory /home/debian/vaultnode
```

The command creates a tiny random physical pack, uploads it through the local
Bot API, downloads it again, verifies the exact bytes and BLAKE3/pack framing,
then deletes only the temporary Telegram message. A successful output ends
with `telegram_smoke=PASS`. This is deliberately a tiny connectivity gate;
the separate 512 MiB production-sized smoke must still be recorded before
calling the COLD provider staging-validated.

## Readiness and recovery

Run `docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T worker launcher-admin storage health` before
the live smoke. Keep the Telegram message/index state on the persistent
`launcher-storage` volume. Do not claim COLD-to-HOT recovery is validated until
a physical pack is deliberately removed from FileMirage HOT, restored from
Telegram through the worker, BLAKE3-verified, and published back to HOT.
