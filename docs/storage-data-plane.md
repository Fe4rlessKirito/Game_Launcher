# Storage data plane

The control plane stores manifests, logical chunk identity, pack metadata,
provider pools, and verified locations. It does not proxy normal game bytes.

```text
manifest -> logical chunk hashes -> pack resolver -> direct HOT URL
                                          \-> legacy chunk URL fallback
```

The signed manifest stays provider-independent. `POST
/api/v1/builds/{build_id}/packs/resolve` is an additive, build-scoped
resolution endpoint. It returns only HOT sources and safe capability metadata;
COLD locations are never returned to clients. If a requested pack has only
COLD locations, the endpoint queues a server-side pack restore and returns
`restore_pending`.

The launcher downloads a pack directly, verifies its BLAKE3 identity and
index, extracts only the requested logical chunks, verifies each encoded hash,
and places those chunks in the existing install cache. The pack itself is a
bounded acceleration cache and may be evicted after a lease expires.

`GET /api/v1/storage/providers` exposes provider capabilities for operators.
`launcher-admin storage probe` is offline by default; `--live` is an explicit
network/credential health probe.

The physical schema is additive: `physical_packs`, `pack_chunks`,
`pack_locations`, `pack_restore_jobs`, and `pack_leases` coexist with the
legacy logical-object tables.
