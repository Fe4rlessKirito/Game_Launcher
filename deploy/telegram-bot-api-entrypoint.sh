#!/bin/sh
set -eu

: "${TELEGRAM_API_ID:?TELEGRAM_API_ID is required}"
: "${TELEGRAM_API_HASH:?TELEGRAM_API_HASH is required}"

port="${PORT:-8081}"
data_dir="${TELEGRAM_BOT_API_DIR:-/var/lib/telegram-bot-api}"
temp_dir="${TELEGRAM_BOT_API_TEMP_DIR:-/tmp/telegram-bot-api}"
mkdir -p "$data_dir" "$temp_dir"

exec /usr/local/bin/telegram-bot-api \
    --api-id="$TELEGRAM_API_ID" \
    --api-hash="$TELEGRAM_API_HASH" \
    --local \
    --dir="$data_dir" \
    --temp-dir="$temp_dir" \
    --http-port="$port"
