# Generic capacity provisioning architecture

Capacity provisioning is an account-lifecycle subsystem around the existing
storage pools. It does not replace FastCDC chunking, BLAKE3 verification, Zstd
encoding, manifests, HOT/COLD placement, or launcher download behavior.

```text
publication / health signal
          |
          v
CapacityManager -- one active job per provider/pool -- PostgreSQL
          |
          +--> CapacityProvisioner (manual or automatic)
          |        returns candidate material only
          |
          +--> authoritative validator
          |        auth, capacity, tiny upload/download/BLAKE3/delete
          |
          +--> candidate enroller --> existing storage account ledger/pool
          |
          +--> blocked work wakes on the next capacity/worker pass
```

`ProvisionerCapabilities` declares provider type, supported pool classes,
manual/automatic/disabled mode, email/operator requirements, capacity-query
support, and reauthentication support. A provisioner may create or retrieve a
candidate in an external provider, but it never inserts a storage account,
marks a pool ready, or changes placement membership. Those operations happen
only after the server-owned validation and enrollment stages.

The current MEGA binding is intentionally non-operative for signup: it creates
a `NEEDS_OPERATOR` job whose action points to the existing
`launcher-admin storage accounts add` workflow. The fake automatic binding is
used for deterministic end-to-end tests and future hook integration.
