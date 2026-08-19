# Storage data plane

The control plane stores manifests, logical chunk identity, pack metadata,
provider pools, and verified locations. Normal current-build bytes are still
downloaded directly from HOT providers. Historical Telegram packs use the
bounded private-worker relay described below.

```text
manifest -> logical chunk hashes -> pack resolver -> direct HOT URL
                                          \-> legacy chunk URL fallback
```

The signed manifest stays provider-independent. `POST
/api/v1/builds/{build_id}/packs/resolve` is the canonical build-scoped
resolution endpoint when pack storage is enabled. The newest build receives
only direct HOT pack sources. An older build may receive an API-owned,
build-scoped COLD stream URL; it never receives a Telegram URL, message ID,
credential, or provider location. If the private stream worker is not
configured, the compatibility path queues a server-side HOT restore and
returns `restore_pending`.

The launcher downloads a pack directly, verifies its BLAKE3 identity and
index, extracts only the requested logical chunks, verifies each encoded hash,
and places those chunks in the existing install cache. The pack itself is a
bounded temporary cache and may be evicted after the install operation.

For updates and repairs, the launcher first asks the pack resolver only for
chunks that are absent from its verified local cache. If the missing logical
bytes are below `LAUNCHER_PACK_SPARSE_RELAY_THRESHOLD` of a candidate HOT pack
(default `0.5`), it uses `POST /api/v1/builds/{build_id}/resolve` and receives
an API-owned chunk URL. The API reads only the indexed byte range from a
verified HOT physical pack, checks the encoded BLAKE3 hash, and returns that
chunk. When the missing ratio is above the threshold, the launcher downloads
the full pack directly from FileMirage. This keeps full installs and large
updates on the direct path while avoiding a full-pack transfer for small
repairs.

The sparse relay is build-scoped and only serves hashes present in a published
latest-build manifest. It never exposes FileMirage credentials, provider URLs,
or Telegram metadata to the launcher. Historical builds continue to use the
bounded COLD pack stream because Telegram is not a client-facing range source.

`GET /api/v1/storage/providers` exposes provider capabilities for operators.
`launcher-admin storage probe` is offline by default; `--live` is an explicit
network/credential health probe.

Build history is retained independently of the normal HOT path. The resolver
uses HOT locations and configured mirrors only for the newest published build.
For an older build, the launcher downloads the required pack through the API
relay: Telegram COLD -> private worker -> API -> launcher. The worker applies
backpressure and deletes no Telegram data; it also does not persist a permanent
HOT copy. The launcher verifies the pack BLAKE3 and may retain the verified
bytes in its normal local cache. Telegram remains the historical source of
record, while the newest build is the only version maintained as normal HOT
traffic.

The physical schema is additive: `physical_packs`, `pack_chunks`,
`pack_locations`, `pack_restore_jobs`, `pack_leases`, and `build_packs` coexist
with the legacy logical-object tables. In pack-canonical mode, those legacy
tables are metadata/compatibility tables and are not required to contain byte
replicas for publication.
