# ADR 0009: direct HOT downloads

The API resolves metadata and ranked HOT source URLs. Clients download pack or
logical chunk bytes directly, verify hashes locally, refresh expiring URLs via
the resolver, and fall back across sources. COLD providers and private
credentials never cross the API/client boundary.
