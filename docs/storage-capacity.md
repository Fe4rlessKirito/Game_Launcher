# Storage capacity and reservations

Cold accounts are modeled as capacity members of a provider pool, not as one
oversized logical disk. `storage_pools` identifies the logical class/provider
pool and failure domain. `storage_accounts` stores capacity, used bytes, held
bytes, safety margin, and operator-visible status. `storage_reservations`
stores the chunk, byte count, expiry, and lifecycle state (`HELD`, `COMMITTED`,
`RELEASED`, or `EXPIRED`).

Before a cold upload, the worker:

1. Locks the account row and expires stale holds.
2. Checks `capacity - used - reserved - safety_margin`.
3. Inserts a hold with a bounded TTL.
4. Uploads and verifies the object.
5. Commits the hold to used bytes, or releases it on failure.

The same operation is implemented by the in-memory ledger used by fake tests,
so concurrency behavior is exercised without real cloud credentials. The
PostgreSQL implementation is the production authority; account rows are locked
with `FOR UPDATE`, and a partial unique index prevents two active reservations
for the same account/chunk.

A full pool returns typed `NeedsCapacity` and leaves the build unpublished; it
does not silently delete content or lower the policy. The pool-level
`StorageCapacityProvisioner` seam supports `DISABLED`, `MANUAL`, and
`AUTOMATIC` modes. The current MEGA implementation is
`ManualStorageCapacityProvisioner`: it reports `NEEDS_CAPACITY` until an
operator enrolls another account through the existing admin command.

An automatic provisioner may later return an account/credential reference. It
does not perform consumer signup. The existing authentication, capacity query,
smoke test, enrollment, and `ACTIVE` transition remain the safety boundary.

Operators can inspect capacity without exposing credentials:

```powershell
launcher-admin storage accounts list
launcher-admin storage accounts inspect --account-id mega-a
launcher-admin storage pools list
launcher-admin storage health
```

The storage status API reports account capacity and pool/provider health but
never returns credential values or MEGA session material.
