# Storage classes, pools, and publication policy

The control plane treats storage placement as a policy decision. A logical
`StorageClass` is independent from a provider:

```text
HOT     -> Railway/S3 pool
COLD    -> MEGA pool (accounts A, B, C)
ARCHIVE -> reserved for a future provider
```

A `StoragePool` has an ID, class, provider type, deterministic priority,
failure domain, enabled flag, status, and provisioning mode. Multiple pools may
serve the same class. MEGA accounts are capacity members of one pool; they do
not create independent provider failure domains.

`StorageTier` remains a source-compatible alias for `StorageClass` while
operators migrate scripts.

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
`LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED`, and use `PROACTIVE` after the MEGA
pool has been validated. Publication is rejected until every manifest chunk
satisfies both replica and failure-domain requirements. A build remains
`READY` when placement fails; an operator can correct pool health/capacity and
rerun the publish command.

Placement is deterministic: existing verified locations count first, then
enabled/healthy pools are ordered by class, pool priority, and pool ID. A pool
whose available capacity is below the encoded chunk size is skipped. The
engine tracks replica count and distinct failure domains separately. Capacity
reservations are held before an upload and committed only after size/hash
verification, preventing concurrent publishers from overcommitting an account.

The API resolver only returns hot locations. Cold records are used to enqueue a
restore job, and the client receives `503` with code `restore_pending` and a
`Retry-After` header. Cold provider credentials, paths, and download URLs are
never exposed to clients.

Before staging traffic is enabled, run:

```powershell
launcher-admin storage readiness --storage-root C:\launcher\storage
```

This checks policy-required healthy pools and failure domains, a
database-backed cold account pool, and usable capacity. `storage pools list`
and `storage pools inspect <id>` show the same pool metadata and health
ordering used by placement.
