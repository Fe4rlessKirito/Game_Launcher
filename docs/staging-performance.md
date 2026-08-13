# Staging performance record

This document is a measurement sheet, not a result claim. Fill it from the
same Railway staging environment after the live validation gate is approved.
Do not substitute local mock-provider or localhost numbers for remote results.

## Environment record

Record:

- Railway region and environment name
- API deployment ID and git commit
- API/worker CPU and memory limits
- PostgreSQL plan, version, connection pool size
- Railway Bucket region, URL style, and object prefix
- Telegram Local Bot API version/mode and worker state-volume size
- launcher build, OS, network path, and client concurrency
- synthetic A/B manifest IDs, encoded bytes, raw bytes, chunk count, and cache state

Do not record credentials, session tokens, private keys, or full presigned URLs.

## Required measurements

Measure each case at least three times and report median and p95:

1. API liveness, readiness, catalog, resolve, and storage-status latency.
2. Direct HOT bucket download throughput from the launcher. The API must not
   proxy chunk bytes; capture the resolved host and status code without saving
   the query string.
3. First install of synthetic build A: total encoded bytes, network bytes,
   elapsed time, chunks downloaded, and reconstructed hash.
4. Update A to B: reused installed bytes, reconstructed bytes, network bytes,
   elapsed time, and byte-identical output.
5. Resume after an interruption: partial file length, Range request offset,
   206 response, final hash, and bytes transferred after resume.
6. Presigned URL expiry: wait for expiry on one chunk, confirm chunk-level
   resolve refresh, and record the new expiry without logging the URL.
7. Cold Telegram pack upload, verify, download, test deletion, and
   worker-to-HOT restore
   throughput. Record bounded temporary storage peak.
8. Restore-pending latency: API response with Retry-After, worker claim,
   restore completion, and the next successful launcher resolve.

The launcher already records A/B savings as:

    savings = (total_encoded_bytes - network_bytes) / total_encoded_bytes

The earlier local synthetic baseline was approximately 85.34% savings. That is
not remote Railway evidence and must not be copied into the remote result.

## Failure and concurrency measurements

Run one failure at a time:

- bad secondary mirror, then Railway HOT primary;
- Railway HOT failure, then bad secondary mirror;
- an interrupted multipart upload and orphan cleanup;
- API, worker, and PostgreSQL restart/reconnect;
- Telegram Local Bot API and worker restart/reconnect;
- cold outbound network/auth failures without credential retry loops.

Repeat the small synthetic smoke at normal launcher concurrency. Record API
requests per second, active downloads, worker jobs, PostgreSQL connections,
bucket operations, CPU, memory, and disk/volume usage. Confirm that concurrent
jobs remain bounded and that temporary chunk storage returns to its baseline.

## Result table

| Case | Runs | Median | P95 | Evidence | Status |
| --- | ---: | ---: | ---: | --- | --- |
| API /v1/health | pending | pending | pending | HTTP status | NOT RUN |
| API /v1/ready | pending | pending | pending | DB SELECT 1 | NOT RUN |
| HOT direct download | pending | pending | pending | client-to-bucket trace | NOT RUN |
| A first install | pending | pending | pending | hashes and bytes | NOT RUN |
| A to B update | pending | pending | pending | savings and hashes | NOT RUN |
| Resume/range | pending | pending | pending | Range/206 trace | NOT RUN |
| Presign refresh | pending | pending | pending | two resolves | NOT RUN |
| Telegram 512 MiB smoke | pending | pending | pending | operator log | NOT RUN |
| Cold restore | pending | pending | pending | job + hot hash | NOT RUN |
| Restart/reconnect | pending | pending | pending | deployment logs | NOT RUN |
