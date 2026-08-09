# Cold restoration

Cold objects are never returned as client download URLs. When a published
chunk has cold coverage but no verified hot location, the resolver enqueues a
deduplicated `restore_jobs` row and returns:

```json
{
  "code": "restore_pending",
  "message": "the chunk is in cold storage and a hot restore has been queued"
}
```

The response is HTTP `503` with `Retry-After: 30`. This keeps the client
protocol simple: retry resolution, then download from a normal hot URL.

The worker restore path claims jobs with a lease, reads from configured cold
providers, verifies BLAKE3, writes to a selected hot provider, records the
verified hot object/location, and marks the job `DONE`. Failures are recorded
with attempts and a retry state; expired leases are recoverable by another
worker. `ON_DEMAND` is the development default. `PROACTIVE` is suitable for
staging after a cold pool has been validated and can be scheduled by an
operator worker.

```powershell
launcher-admin storage restore <encoded-blake3-hash>
launcher-admin storage restore-pending --limit 100
```

Restoration is server-side and does not use API request threads for byte
transfers. It is intentionally explicit: a missing cold object or a corrupt
download produces a failed/retryable job rather than publishing or serving
unverified bytes.
