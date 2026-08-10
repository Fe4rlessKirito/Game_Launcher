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
protocol simple: retry resolution, then download from a normal HOT URL.

The worker restore path claims jobs with a lease, ranks enabled source pools by
class and priority, tries each available verified source, verifies BLAKE3,
writes to a selected HOT pool, records the verified HOT object/location, and
marks the job `DONE`. Logs include the selected source pool and failure domain.
Failures are recorded with attempts and a retry state; expired leases are
recoverable by another worker. `ON_DEMAND` is the development default.
`PROACTIVE` is suitable for staging after a cold pool has been validated and
can be scheduled by an operator worker.

The launcher still receives HOT-only locations. It never receives a COLD pool
ID, MEGA path, credential, or cold URL.

```powershell
launcher-admin storage restore <encoded-blake3-hash>
launcher-admin storage restore-pending --limit 100
```

Restoration is server-side and does not use API request threads for byte
transfers. A missing cold object or corrupt download produces a failed and
retryable job rather than publishing or serving unverified bytes.
