# Telegram COLD staging runbook

1. Create or select the operator-owned target chat and obtain its numeric ID.
2. Add `TELEGRAM_BOT_TOKEN` and `TELEGRAM_COLD_CHAT_IDS` to the private worker
   only. Put `TELEGRAM_COLD_STATE_FILE` on the small persistent worker volume.
3. Set `LAUNCHER_STORAGE_PROVIDERS` to include `telegram` and keep
   `TELEGRAM_COLD_ENABLED`/the deployment feature flag off until the offline
   probe is clean.
4. Run `launcher-admin storage probe --provider telegram --live` from the
   private worker environment.
5. Run the fake-provider suite before a real smoke: upload a tiny random pack,
   restore it, verify BLAKE3, and delete it.
6. Only then run a single operator-approved real pack smoke. Confirm the bot
   message reference is persisted without the token and that the launcher sees
   no Telegram URL.

Do not paste a bot token or state file contents into chat.
