# Ingestion

Ingestion is a staged, idempotent operation. Analyzer output is validated before packager input. Packaging writes objects to a staging directory and emits a publisher report with raw size, encoded size, deduplication, files, chunks, support findings, and warnings. An operator must explicitly publish a `READY` build.

The optional server-side release scraper is a separate pre-ingest boundary.
It accepts only explicitly registered, authorized HTTP(S) sources, discovers a
normalized release, downloads and validates one artifact, and writes a
`handoff.json`. The operator then passes that artifact to the existing bounded
normalizer/analyzer/packager path:

```text
launcher-scraper ingest <source> --output <artifact-root>
launcher-admin ingest <handoff-artifact> --output <package-dir>
```

The scraper never publishes a build and never makes the launcher client depend
on a source website. See [scraper.md](scraper.md) for browser isolation,
Gemini budgets, SSRF protection, archive validation, and restart-safe jobs.

On Mantle, the scraper artifact and the Rust package are temporary staging
data. When `LAUNCHER_CLEANUP_STAGING_AFTER_PUBLISH=true`, the publish command
deletes the package, copied staging chunks, and the recorded source artifact
only after the HOT/COLD storage policy has completed successfully. A provider
or database failure leaves the staging data in place so the operator can retry.
