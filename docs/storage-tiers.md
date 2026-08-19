# Storage classes, pools, and publication policy

The control plane treats storage placement as a policy decision. A logical
`StorageClass` is independent from a provider:

```text
HOT     -> FileMirage direct-download pool
COLD    -> Telegram pack store (operator-owned channel)
ARCHIVE -> reserved for a future provider
```

A `StoragePool` has an ID, class, provider type, deterministic priority,
failure domain, enabled flag, status, and provisioning mode. Multiple pools may
serve the same class. Telegram is the required staging COLD pool. Other COLD
adapters, including MEGA, remain optional and are not part of the staging gate.

`StorageTier` remains a source-compatible alias for `StorageClass` while
operators migrate scripts. Mantle is the active API/worker deployment target;
FileMirage is the active HOT data plane and Telegram is the retained COLD
store. Local and S3-compatible pools remain supported compatibility providers,
and S3-compatible pools remain optional compatibility providers.

The relevant environment variables are:

| Variable | Meaning |
| --- | --- |
| `LAUNCHER_STORAGE_MIN_HOT_REPLICAS` | Minimum verified hot replicas per chunk. |
| `LAUNCHER_STORAGE_MIN_COLD_REPLICAS` | Minimum verified cold replicas per chunk. |
| `LAUNCHER_STORAGE_MIN_ARCHIVE_REPLICAS` | Minimum verified archive replicas; default `0`. |
| `LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS` | Target hot placement count. |
| `LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS` | Target cold placement count. |
| `LAUNCHER_STORAGE_PREFERRED_ARCHIVE_REPLICAS` | Target archive placement count; default `0`. |
| `LAUNCHER_STORAGE_MIN_HOT_FAILURE_DOMAINS` | Minimum independent hot domains; default `1`. |
| `LAUNCHER_STORAGE_MIN_COLD_FAILURE_DOMAINS` | Minimum independent cold domains; default `0`. |
| `LAUNCHER_STORAGE_MIN_ARCHIVE_FAILURE_DOMAINS` | Minimum independent archive domains; default `0`. |
| `LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED` | Forces at least one cold replica when true. |
| `LAUNCHER_STORAGE_RESTORE_MODE` | `ON_DEMAND` or `PROACTIVE`. |

Development defaults are one hot replica and one hot failure domain, with no
cold or archive requirement. Staging should set one hot and one cold minimum,
one failure domain for each class, enable
`LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED`, and use `PROACTIVE` after Telegram
pack storage has been validated. Publication is rejected until every manifest chunk
satisfies both replica and failure-domain requirements. A build remains
`READY` when placement fails; an operator can correct pool health/capacity and
rerun the publish command.

Placement is deterministic: existing verified locations count first, then
enabled/healthy pools are ordered by class, pool priority, and pool ID. A pool
whose available capacity is below the encoded chunk size is skipped. The
engine tracks replica count and distinct failure domains separately. Capacity
reservations are held before an upload and committed only after size/hash
verification, preventing concurrent publishers from overcommitting an account.

The API resolver returns HOT locations for normal current-build traffic. For an
older build with physical packs enabled, it may return an API-owned
build-scoped COLD stream URL backed by the private worker. Cold provider
credentials, paths, message IDs, and download URLs are never exposed to
clients. If the stream worker is unavailable, the compatibility resolver
queues a restore job and returns `503` with code `restore_pending` and a
`Retry-After` header.

## Build history retention

Published builds are immutable history. Publishing build `B` for a game does
not delete build `A` from the manifest database or from Telegram COLD:

```text
latest build B  -> normal HOT resolution and mirrors
older build A   -> Telegram COLD -> private worker -> API stream -> launcher
```

Only the newest published build is eligible for normal HOT resolution. When a
new build is published, HOT objects and HOT pack locations that are unique to
superseded builds are retired. Content-addressed bytes shared with the newest
build remain HOT. Telegram COLD packs are never removed by this retention pass
or by unreachable-object garbage collection; `build_packs` keeps the historical
build-to-pack relationship, including a migration backfill for existing packs.

An old build can therefore still be selected and downloaded, but it is served
through a server-side Telegram stream, never through a Telegram URL or
credential in the launcher. The stream is backpressured and does not create a
permanent HOT copy. The compatibility restore path remains available when the
private stream worker is not configured.

If a superseded HOT provider is not configured during publication, retention is
reported as partial and the provider is left untouched rather than silently
deleting database records for bytes the worker cannot delete.

Before staging traffic is enabled, run:

```powershell
launcher-admin storage readiness --storage-root C:\launcher\storage
```

This checks policy-required healthy pools and failure domains, a
database-backed cold account pool, and usable capacity. `storage pools list`
and `storage pools inspect <id>` show the same pool metadata and health
ordering used by placement.
