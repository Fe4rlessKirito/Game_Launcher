# API protocol

All timestamps are UTC RFC 3339. Public catalog responses are paginated and may be cached with an ETag. Mirror resolution is short-lived because URLs can expire.

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/health` | Liveness/readiness probe |
| GET | `/api/v1/games?cursor=&limit=` | Published catalog page |
| GET | `/api/v1/games/{game_id}` | Game metadata and current published builds |
| GET | `/api/v1/builds/{build_id}/manifest` | Published manifest |
| POST | `/api/v1/builds/{build_id}/resolve` | Resolve direct storage URLs for a batch of chunks |

The server returns typed errors with `code`, `message`, and optional `request_id`. A client may retry idempotent GETs and chunk resolution; chunk providers may be retried independently.
