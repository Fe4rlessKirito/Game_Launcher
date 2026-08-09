# Ingestion

Ingestion is a staged, idempotent operation. Analyzer output is validated before packager input. Packaging writes objects to a staging directory and emits a publisher report with raw size, encoded size, deduplication, files, chunks, support findings, and warnings. An operator must explicitly publish a `READY` build.
