# Storage capacity and reservations

Cold accounts are modeled as a pool, not as one oversized logical disk.
`storage_accounts` stores capacity, used bytes, held bytes, safety margin, and
operator-visible status. `storage_reservations` stores the chunk, byte count,
expiry, and lifecycle state (`HELD`, `COMMITTED`, `RELEASED`, or `EXPIRED`).

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

Configure the margin explicitly per account with
`safety_margin_bytes`. Capacity thresholds are not hardcoded. A full pool
returns typed `NeedsCapacity` and leaves the build unpublished; it does not
silently delete content or lower the policy.

Operators can inspect capacity without exposing credentials:

```powershell
launcher-admin storage accounts list
launcher-admin storage accounts inspect --account-id mega-a
launcher-admin storage health
```

The storage status API reports account capacity and pool/provider health but
never returns credential values or MEGA session material.
