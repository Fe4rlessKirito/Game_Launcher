# Storage policy

Storage classes are HOT, COLD, and ARCHIVE. A policy counts verified replicas
and verified failure domains independently. Two providers in one failure
domain do not satisfy a multi-domain requirement.

`StoragePlacementEngine` selects enabled, healthy pools by priority and
capacity. In pack-canonical mode, publication gates on verified physical-pack
coverage and pack locations for the HOT/COLD policy. Logical chunk rows remain
the manifest/index contract but do not count as byte replicas. Legacy
non-canonical publication continues to use the logical chunk policy.

Safe eviction rules are conservative:

- never evict a pinned or leased pack;
- never evict the last verified copy in a failure-domain policy;
- verify another HOT or COLD location before deleting a HOT cache copy;
- treat HOT expiry as cache pressure, not loss of the canonical COLD copy;
- mark a pack under-replicated or degraded when reconciliation finds a policy
  violation.

Relevant environment variables are `LAUNCHER_STORAGE_MIN_*_REPLICAS`,
`LAUNCHER_STORAGE_MIN_*_FAILURE_DOMAINS`, and the preferred replica settings.
