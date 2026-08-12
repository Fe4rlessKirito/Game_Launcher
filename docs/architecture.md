# Architecture

## Boundaries

Launcher has a control plane and a data plane. The API owns catalog state, build lifecycle, signed metadata, and mirror resolution. `StorageRegistry` owns ordered replication across local and S3-compatible providers. Storage providers own bytes. A client receives expiring or stable object URLs and downloads current-build chunks directly from a provider. Historical COLD packs use a bounded, authenticated server relay because Telegram is never a client source.

```text
Avalonia client ── catalog/manifests ──> Axum API ──> PostgreSQL
      │                                      │
      └──────── direct chunk bytes ──────────┴──> local/S3 storage

authorized directory/archive ──> bounded normalizer ──> Python analyzer ──> Rust packager ──> storage + DB
```

The analyzer produces facts and candidates. The packager consumes a versioned analysis report and creates deterministic content-addressed objects. Publication is an explicit operator transition.

## Lifecycle

Builds use the following monotonic operational states:

`DISCOVERED → ANALYZED → PACKAGED → UPLOADED → VERIFIED → READY → PUBLISHED`

Failures are recorded with an error and can be retried from the last idempotent boundary. A build is not visible to normal catalog queries until it is `PUBLISHED`.

## Client responsibilities

The client owns UI, local SQLite state, download scheduling, cache eviction, reconstruction, transactional installation, repair, launch and self-update coordination. Long work is cancellable and never runs on the UI thread. Installed manifests are retained locally so an offline client can launch and uninstall games after a server build is unpublished.

## Security boundaries

- Manifest paths are portable relative paths and are validated before any filesystem operation.
- Encoded and raw chunk hashes are both verified before content is promoted into cache or installation.
- Launching uses an executable path and an argument list, never a shell command string.
- Signed manifests and updater releases are separate from transport TLS.
- Provider credentials remain server-side. The client receives only scoped object URLs.

## Deliberate v1 decisions

1. PostgreSQL-backed jobs are sufficient for the first ingestion worker. The job table has leases and retries; Redis, Kafka, and RabbitMQ are intentionally absent.
2. Local filesystem storage is the development provider. `StorageProvider`, `StorageRegistry`, and the verified `storage_locations`/`storage_objects` records are the seam for S3-compatible storage and independent mirrors.
3. Storage placement is tier-aware: hot locations are client-facing, cold locations are operator-facing, and publication is gated by `StoragePolicy`. MEGA and Telegram cold accounts use PostgreSQL-backed metadata; historical Telegram packs can be streamed through the private worker without creating a permanent HOT copy.
4. The manifest is JSON for inspectability and signed canonical bytes can be introduced without changing the file/chunk model.
5. SQLite uses numbered SQL migrations and a small repository abstraction instead of an ORM.
6. Deployment is provider-neutral. Railway is the current staging host; Mantle is a future deployment target, not a storage or database type in the application. The API, worker, PostgreSQL schema, storage pools, and client contracts remain portable across that move.

## Known limitations

The current infrastructure phase includes local and S3-compatible providers, multipart/retry/hash verification, presigned URL generation, provider health reporting, and an operator publish path. It does not yet ship a production key-management service, a continuously running ingestion worker, Windows named-pipe single-instance broker, or native-AOT release pipeline. Those remain explicit extension points and are not silently represented as complete.
