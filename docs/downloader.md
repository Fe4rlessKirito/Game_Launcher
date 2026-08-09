# Downloader

The client downloader schedules bounded chunk work through a global semaphore and a per-provider semaphore. Every chunk progresses through `QUEUED`, `RESOLVING`, `DOWNLOADING`, `VERIFYING_ENCODED`, `DECOMPRESSING`, `VERIFYING_RAW`, and `READY`. Retries use exponential backoff with jitter and move to the next resolved mirror after provider failure.

Part files live below `cache/tmp/<encoded-hash>.part`. Range resume is attempted only when a provider advertises stable range support; a failed resume restarts that chunk rather than the entire job. UI progress is sampled at a fixed interval from moving-average byte counters.
