# Private Telegram Local Bot API on Railway

The 512 MiB COLD smoke uses a separate private Railway service running the
official [Telegram Bot API server](https://github.com/tdlib/telegram-bot-api)
in `--local` mode. It is not part of the restore-worker image and it receives
no public domain.

## Image build and service

The repository contains a pinned-source Dockerfile for the official
[Telegram Bot API server](https://github.com/tdlib/telegram-bot-api), but the
native C++ build should not run on a small Railway builder. The
`telegram-bot-api-image.yml` GitHub Action builds the pinned source on a
GitHub-hosted runner and publishes a `linux/amd64` image to GHCR:

```text
ghcr.io/fe4rlesskirito/game-launcher-telegram-bot-api:sha-<main-commit>
```

Connect the private Railway service to that immutable image after the Action
finishes. Keep `railway.telegram-bot-api.toml` as the source-build fallback;
do not use it for the normal Railway staging deployment. The image listens on
Railway's injected `PORT` (default `8081`) and has no HTTP healthcheck because
authenticated `getMe` is the useful readiness check.

The image is linked to this public repository by its OCI source label. If GHCR
does not make the package public automatically, change only that package's
visibility to public; the image contains no Telegram credentials.

Attach a persistent volume at `/var/lib/telegram-bot-api`. This is Bot API
state and its local file working directory, not the launcher chunk store. The
`/tmp/telegram-bot-api` directory is ephemeral and should be sized for the
largest in-flight file plus operating headroom.

Set these secrets only on the Local Bot API service:

```text
TELEGRAM_API_ID=<operator-provided api_id>
TELEGRAM_API_HASH=<operator-provided api_hash>
```

The official server accepts HTTP and defaults to port 8081. Local mode allows
unlimited downloads and uploads up to 2000 MB; the service should therefore
remain private to Railway networking.

## Restore-worker wiring

Set these only on the private restore worker:

```text
TELEGRAM_BOT_API_BASE_URL=http://<local-api-private-host>:8081
TELEGRAM_BOT_TOKEN=<Railway secret>
TELEGRAM_COLD_CHAT_IDS=<operator-selected numeric chat id>
TELEGRAM_COLD_ENABLED=true
TELEGRAM_COLD_MAX_UPLOAD_BYTES=536870912
TELEGRAM_COLD_STATE_FILE=/var/lib/launcher/telegram/telegram-cold-state.json
LAUNCHER_STORAGE_PROVIDERS=s3,telegram
LAUNCHER_COLD_STREAM_TOKEN=<same sealed secret as the API>
# Optional; omit this and the worker binds 0.0.0.0:$PORT automatically.
```

Create the worker volume directory used by `TELEGRAM_COLD_STATE_FILE` and
keep its existing MEGAcmd session directory separate. The API service remains
`LAUNCHER_STORAGE_PROVIDERS=s3`; Telegram is never returned to launcher
clients.

The exact private hostname is the Railway internal hostname assigned to the
Local Bot API service. Do not use a public domain or commit the resolved
hostname if Railway changes it.

Set these only on the API service:

```text
LAUNCHER_STORAGE_PROVIDERS=s3
LAUNCHER_COLD_STREAM_WORKER_URL=http://<restore-worker-private-host>:<worker-port>
LAUNCHER_COLD_STREAM_TOKEN=<same sealed secret as the worker>
```

The API uses the worker URL only for superseded-build pack streams. The worker
authenticates the internal request, reads Telegram through the private Local
Bot API, and streams the body back. No Telegram URL or credential is exposed,
and no restored pack is written to HOT.

## First checks

From the worker environment, run:

```text
launcher-admin storage health
```

Then run the real 512 MiB pack smoke, verify BLAKE3, stream it through the
private worker/API path, and record the 1/2/4/8/16 restore timings before
enabling the COLD publication gate. Keep the Telegram message; only transient
HTTP response state and any opted-in local cache copy may be discarded. Never
paste `TELEGRAM_BOT_TOKEN`, `TELEGRAM_API_HASH`, or the state file into chat or
Git.
