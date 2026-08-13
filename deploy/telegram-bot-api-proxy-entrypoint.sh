#!/bin/sh
set -eu

: "${TELEGRAM_API_ID:?TELEGRAM_API_ID is required}"
: "${TELEGRAM_API_HASH:?TELEGRAM_API_HASH is required}"

public_port="${PORT:-8080}"
local_api_port="${TELEGRAM_LOCAL_API_PORT:-8081}"
data_dir="${TELEGRAM_BOT_API_DIR:-/var/lib/telegram-bot-api}"
temp_dir="${TELEGRAM_BOT_API_TEMP_DIR:-/tmp/telegram-bot-api}"
mkdir -p "$data_dir" "$temp_dir"

/usr/local/bin/telegram-bot-api \
    --api-id="$TELEGRAM_API_ID" \
    --api-hash="$TELEGRAM_API_HASH" \
    --local \
    --dir="$data_dir" \
    --temp-dir="$temp_dir" \
    --http-port="$local_api_port" &
bot_pid=$!

cleanup() {
    kill "$bot_pid" 2>/dev/null || true
    wait "$bot_pid" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

exec python3 /usr/local/bin/telegram-bot-api-file-proxy.py \
    --listen="$public_port" \
    --upstream-host="${TELEGRAM_BOT_API_UPSTREAM_HOST:-127.0.0.1}" \
    --upstream="$local_api_port"
