# ADR 0006: Storage classes, provider pools, and capacity provisioners

## Status

Accepted

## Decision

Model `StorageClass` (`HOT`, `COLD`, `ARCHIVE`) separately from provider pools.
A pool has stable identity, provider type, priority, failure domain, enabled
flag, aggregate status, and provisioning mode. Existing Railway/S3 records map
to a HOT pool; existing MEGA accounts map to one COLD pool. Multiple accounts
inside one pool do not imply independent provider failure domains.

Replica policy therefore has two independent requirements: verified replicas
per class and minimum failure domains per class. Placement and restore rank
compatible pools deterministically by priority, health/availability, capacity,
and failure domain. The launcher remains HOT-only.

Capacity provisioning lives behind a pool-level asynchronous interface. The
current MEGA implementation is manual and returns `NEEDS_CAPACITY`; future
automatic provisioners may supply an enrolled account reference but may not
perform consumer signup.

## Consequences

The 003 forward migration preserves provider/account/location/object/restore
records and backfills deterministic pool and failure-domain links. Provider
implementations remain focused on bytes and health. The policy, placement,
restore, readiness, metrics, and operator surfaces can now grow to multiple
pools without changing chunking, BLAKE3, Zstd, launcher networking, or API
client resolution semantics.
