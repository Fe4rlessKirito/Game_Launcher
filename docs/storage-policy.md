# Storage policy operations

`StoragePolicy` is class-based. It can require minimum and preferred verified
replicas for `HOT`, `COLD`, and future `ARCHIVE`, plus optional minimum failure
domains per class. A failure domain is an outage boundary, not an account:
two MEGA accounts in the same MEGA pool count as two capacity members but one
provider failure domain.

Placement consumes the object size, existing verified locations, available
pools, pool health/status, priority, capacity, and failure domains. It returns
explicit actions containing the provider, pool, class, priority, and failure
domain. Disabled, unavailable, unhealthy, and full pools are skipped. If the
required class coverage cannot be projected, the explanation identifies the
missing replica/domain counts and publication stays gated.

The database publication check repeats the same invariant from persisted
verified object/location records. It counts distinct verified provider copies
for replicas and distinct failure domains for the domain requirement, so a restart cannot turn
an in-memory placement decision into an unsafe publish.

Useful operator commands:

```powershell
launcher-admin storage policy
launcher-admin storage pools list
launcher-admin storage pools inspect <pool-id>
launcher-admin storage readiness
```
