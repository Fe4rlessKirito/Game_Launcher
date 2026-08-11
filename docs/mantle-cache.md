# Mantle elastic cache

Mantle is the bounded local cache used for pack staging, restore downloads,
and extraction scratch space. It is not the canonical chunk store. Configure
its preferred/minimum bytes and free-disk safety margins with
`LAUNCHER_MANTLE_CACHE_*` variables.

Before a restore or extraction, the worker reserves the estimated bytes. A
lease has a bounded expiry and is reconciled after restart. Eviction considers
only unpinned, unleased packs with a verified location elsewhere; it is
ordered by least-recent access. Emergency free-disk thresholds trigger
eviction before new work is accepted. Temporary COLD downloads use the same
bounded scratch policy and are deleted after verification/upload.

The in-process `MantleCache` implementation is covered by tests for bounded
reservations, lease reconciliation, and safe eviction. PostgreSQL
`pack_leases` provides durable coordination for server-side pack restores.
