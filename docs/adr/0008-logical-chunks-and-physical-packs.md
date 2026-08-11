# ADR 0008: logical chunks and immutable physical packs

Logical FastCDC chunks remain the manifest and deduplication contract.
Immutable physical packs are a separate transport/storage artifact with their
own BLAKE3 identity and index. This permits provider changes, bounded packing,
direct HOT transfer, and server-side COLD restore without changing signed
manifests or invalidating legacy chunk objects.
