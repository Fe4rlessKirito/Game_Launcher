# ADR 0008: logical chunks and immutable physical packs

Logical FastCDC chunks remain the signed manifest, diff, and deduplication
contract. Immutable physical packs are the canonical byte-storage and
transport artifact, with their own BLAKE3 identity and index. The launcher
downloads a verified pack and materializes requested encoded chunks locally;
the API is responsible for pack resolution and the private worker handles
COLD-to-HOT restoration. This avoids storing every encoded chunk twice while
preserving the existing manifest model and legacy migration path.
