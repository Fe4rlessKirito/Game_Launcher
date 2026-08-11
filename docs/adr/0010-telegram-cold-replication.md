# ADR 0010: Telegram as server-side COLD replication

Telegram Bot API is an operator-configured COLD backend for immutable packs.
Message/file references are persisted in private worker state; restore uses
Bot API `getFile` and uploads HOT after verification. Telegram links are not
client sources. Separate target chats are separate provider records but must
declare their actual failure domain rather than being treated as independent
without operator evidence.
