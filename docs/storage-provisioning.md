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
