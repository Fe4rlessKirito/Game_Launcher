# Storage tiers and publication policy

The control plane treats storage placement as a policy decision. `HOT` is the
client-facing delivery tier; `COLD` is an operator-managed backup tier. Domain
code never assumes that a provider name implies a tier. Each provider declares
its tier, and the policy decides how many verified replicas are required and
how many are preferred.

The relevant environment variables are:

| Variable | Meaning |
| --- | --- |
| `LAUNCHER_STORAGE_MIN_HOT_REPLICAS` | Minimum verified hot providers per chunk. |
| `LAUNCHER_STORAGE_MIN_COLD_REPLICAS` | Minimum verified cold providers per chunk. |
| `LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS` | Target hot placement count. |
| `LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS` | Target cold placement count. |
| `LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED` | Forces at least one cold replica when true. |
| `LAUNCHER_STORAGE_RESTORE_MODE` | `ON_DEMAND` or `PROACTIVE`. |

Development defaults are one hot replica and no cold requirement. Staging should
set one hot and one cold minimum, enable `LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED`
and use `PROACTIVE` once the pool has been validated. Publication is rejected
until every manifest chunk satisfies the configured minimums. A build remains
`READY` when placement fails; an operator can correct provider health/capacity
and rerun the publish command.

Placement is deterministic: existing verified provider records count first, then
healthy candidates are ordered by tier and provider ID. A candidate whose
reported free capacity is below the encoded chunk size is skipped. Capacity
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

This checks policy-required healthy providers, a database-backed cold account
pool, and usable capacity.
