# Benchmarks

The first benchmark fixtures are intentionally synthetic and keep the default FastCDC parameters versioned. Add representative authorized build samples locally, then measure:

- Rust packager raw/encoded throughput and deduplication ratio
- launcher cache hit rate and reconstruction throughput
- update bytes downloaded versus unchanged raw chunk reuse
- API catalog pagination latency

No benchmark result is reported until the fixture and command are recorded alongside the result.
