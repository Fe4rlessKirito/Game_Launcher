# Performance targets

- Render the shell before remote catalog refresh.
- Keep download concurrency bounded and memory proportional to active chunks.
- Coalesce progress notifications to 10 per second or less for UI consumers.
- Stream file reconstruction and hashing.
- Benchmark FastCDC, compression, cache hit rate, update reuse, startup, and API pagination with synthetic fixtures before release.
