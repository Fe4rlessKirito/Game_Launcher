# ADR 0010: Telegram as server-side COLD replication

Telegram Bot API is an operator-configured COLD backend for immutable packs.
Message/file references are persisted in private worker state; restore uses
Bot API `getFile` and, for historical downloads, streams through the private
worker/API relay with BLAKE3 verification. Telegram links are not client
sources and no permanent HOT copy is required. Separate target chats are
separate provider records but must declare their actual failure domain rather
than being treated as independent without operator evidence.
