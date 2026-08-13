#!/bin/sh
set -eu

exec python3 /usr/local/bin/telegram-bot-api-file-proxy.py \
    --listen="${PORT:-8081}" \
    --upstream-host="${TELEGRAM_BOT_API_UPSTREAM_HOST:-telegram-bot-api}" \
    --upstream="${TELEGRAM_LOCAL_API_PORT:-8081}"
