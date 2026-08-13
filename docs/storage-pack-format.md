# Physical pack format

Logical FastCDC chunks remain the content-addressed unit in manifests. A
physical pack is an immutable transport object containing already encoded
logical chunks. Its filename and database identity are the BLAKE3 digest of
the complete pack bytes; the digest is not embedded in the pack.

Version 1 uses:

- a 64-byte little-endian header (`LGRPACK1`, version, entry count, data and
  index bounds);
- concatenated zstd-v1 encoded chunk bytes;
- a fixed 96-byte sorted index entry for each chunk (encoded hash, raw hash,
  offset, encoded length, raw length, compression, flags);
- a 72-byte footer (`LGRPFTR1`) containing the index digest and repeated
  bounds/count values.

Readers reject truncated files, unknown versions/compression, index digest
failures, duplicate or unsorted hashes, integer overflow, out-of-region
offsets, overlapping ranges, oversized declared lengths, pack identity
mismatches, encoded hash mismatches, and raw hash/length mismatches. The Rust
reader is `launcher-packs`; the launcher has an equivalent bounded parser.

Default grouping bounds are target 512 MiB, minimum 256 MiB, and maximum 1
GiB. They are configurable with `LAUNCHER_PACK_TARGET_BYTES`,
`LAUNCHER_PACK_MIN_BYTES`, and `LAUNCHER_PACK_MAX_BYTES`. A final pack may be
smaller than the minimum. A logical chunk is never split across packs.

Legacy `chunks/encoded/{hash}.bin` objects and manifest `object_key` values are
kept in the schema for migration and compatibility. With
`PACK_STORAGE_ENABLED=true`, `LAUNCHER_PACK_CANONICAL=true` makes physical
packs the canonical byte store: publication gates on pack coverage and
verified HOT/COLD pack locations, while logical chunks remain metadata and
FastCDC/deduplication indexes only. The launcher resolves packs, verifies the
pack BLAKE3 identity and structure, and materializes the requested encoded
chunks into its bounded local chunk cache. It falls back to legacy logical
URLs only when pack-canonical mode is disabled or a caller explicitly uses an
older deployment.

`LAUNCHER_PACK_COLD_ONLY=true` makes the COLD policy explicit. Pack mode
requires at least one COLD pack replica, so staging Telegram stores physical
packs rather than a second copy of every logical chunk. HOT redundancy is
carried by physical pack replicas. Set `LAUNCHER_PACK_CANONICAL=false` only
for a migration deployment that still publishes logical HOT objects.
