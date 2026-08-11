# Storage pool provisioning

Provisioning is deliberately a pool-level concern. A storage provider owns
bytes and transfer/health operations; a `StorageCapacityProvisioner` creates
or enrolls additional usable capacity for a pool.

The interface is:

```text
can_provision(required_bytes) -> bool
provision(required_bytes) -> account/credential reference + capacity
```

Supported modes are `DISABLED`, `MANUAL`, and `AUTOMATIC`. The current MEGA
pool uses `MANUAL`. Its provisioner returns `NEEDS_CAPACITY`, preserving the
existing operator account enrollment workflow and avoiding provider-specific
signup automation. When a future automatic module supplies a usable account,
the worker can authenticate, query capacity, run the tiny smoke test, enroll
the account, mark it `ACTIVE`, and resume pending work through the existing
ledger.

Provisioning never changes launcher behavior and never exposes cold
credentials or URLs. Fake provisioners cover successful automatic capacity,
manual `NEEDS_CAPACITY`, and failure paths without external services.

## Account provisioning jobs

The generic account lifecycle is implemented in `launcher-provisioning` and
uses PostgreSQL migration 004. It is intentionally separate from the existing
`StorageCapacityProvisioner` pool hook: a generic `CapacityProvisioner` can
wait for provider email or operator action and return a candidate, while the
server-owned validator/enroller controls admission to the storage ledger.

Use the operator surface to inspect or advance jobs without exposing secret
material:

```powershell
launcher-admin provisioning list
launcher-admin provisioning inspect <job-id>
launcher-admin provisioning readiness
launcher-admin provisioning retry <job-id>
launcher-admin provisioning cancel <job-id>
launcher-admin provisioning complete-manual <job-id> `
  --candidate-reference mega-a `
  --credential-reference secret://mega/a/session `
  --expected-capacity-bytes 1099511627776
```

The manual MEGA provisioner only creates `NEEDS_OPERATOR`; it does not sign up
an account or modify pool membership. `complete-manual` runs the same
authoritative health/capacity and tiny random object validation before the
existing account ledger is marked active.
