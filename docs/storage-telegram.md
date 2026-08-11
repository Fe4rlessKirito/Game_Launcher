# Telegram COLD provider

Telegram COLD is an operator-configured, server-side provider. Set
`TELEGRAM_COLD_ENABLED=true`, `TELEGRAM_BOT_TOKEN`, and
`TELEGRAM_COLD_CHAT_IDS`, and optionally
`TELEGRAM_BOT_API_BASE_URL`, `TELEGRAM_COLD_STATE_FILE`, and
`TELEGRAM_COLD_MAX_UPLOAD_BYTES`. Chat IDs are supplied by the operator; the
worker does not discover chats, create groups, or expose the bot token.

MEGA restores use `LAUNCHER_COLD_TEMP_DIR` and
`LAUNCHER_COLD_TEMP_BYTES` to select a worker-local temporary directory and a
process-wide byte reservation limit. The directory is for in-flight transfer
files only; it is not the persistent MEGAcmd session volume and it is never
used as a chunk store. Each restore or upload releases its reservation after
cleanup.

The provider uploads each physical pack as a document with a deterministic
hash caption and persists only Telegram message/file references in its worker
state file. It uses `sendDocument`, `getFile`, and `deleteMessage`; download
URLs are resolved and consumed by the private restore worker only. Telegram
Bot API file links are never sent to a launcher.

The public Bot API accepts bot uploads up to 50 MB and its `getFile` download
path is limited to 20 MB. That makes the public endpoint unsuitable for a
512 MiB physical pack. Use the official
[Local Bot API Server](https://github.com/tdlib/telegram-bot-api) in `--local`
mode, point `TELEGRAM_BOT_API_BASE_URL` at that private endpoint, and raise
`TELEGRAM_COLD_MAX_UPLOAD_BYTES` only to the pack size the worker has actually
validated. Restore concurrency is bounded by the worker and rate-limit
responses are retried with the provider's retry delay.

For Railway deployment, use the separate private-service setup in
[telegram-local-api-railway.md](runbooks/telegram-local-api-railway.md). The
Local Bot API service owns its own persistent state volume; the restore worker
only owns launcher state, MEGAcmd session state, and bounded temporary restore
space.

Official reference: [Telegram Bot API](https://core.telegram.org/bots/api).
