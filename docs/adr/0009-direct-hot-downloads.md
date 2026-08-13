# ADR 0009: direct HOT downloads

The API resolves metadata and ranked HOT source URLs. In pack-canonical mode,
clients download immutable packs directly, verify the pack identity and index,
extract logical chunks locally, and verify each encoded hash. Legacy
deployments may still download logical chunk bytes directly. Clients refresh
expiring URLs via the resolver and fall back across sources. COLD providers and
private credentials never cross the API/client boundary.
