# Live staging validation checklist

This checklist is the completion gate for the Railway phase. A check is only
PASS when its evidence was captured from the live staging environment. The
repository currently provides the commands and test fixtures, but does not
contain Railway credentials or claim that these live checks have run.

## Deployment and security

- [ ] API service builds from deploy/api.Dockerfile and starts launcher-api.
- [ ] API listens on Railway PORT and /v1/health passes without a database
      or storage scan.
- [ ] /v1/ready passes only after the staging PostgreSQL connection answers
      SELECT 1.
- [ ] PostgreSQL and Restore Worker have no public domains.
- [ ] API has exactly one intended public HTTPS domain; TLS certificate
      validation is enforced by the launcher and operator workstation.
- [ ] Railway Bucket is private, uses separate staging credentials, and is
      wired through Railway variable references.
- [ ] No secret, password, session token, private key, or resolved presigned
      URL appears in logs, source, client binaries, or committed files.
- [ ] Admin actions remain operator-only and authenticated by the deployment
      boundary; no unauthenticated admin API was added.

## Database and storage

- [ ] launcher-admin db status reports CONNECTED and schema_ready.
- [ ] API storage is generic S3 HOT; no Railway-specific provider branch exists.
- [ ] HOT PUT, HEAD, GET, DELETE, multipart upload, abort, and presigned GET
      work against the real Bucket.
- [ ] Bucket download traffic goes directly from the launcher to HOT, not
      through the API byte proxy.
- [ ] The worker has the same PostgreSQL/HOT references and one pre-authenticated
      operator MEGA account; account creation and password automation are absent.
- [ ] MEGAcmd state survives a worker restart on its small persistent volume.
- [ ] MEGA upload, size/hash verification, download, hash verification, and
      delete pass for one synthetic chunk.
- [ ] Outbound MEGA failures are classified as MEGA_NETWORK_UNAVAILABLE or
      MEGA_AUTH_FAILED, with no credential retry loop.
- [ ] Account capacity, safety margin, reservations, and stale-hold cleanup
      remain visible through PostgreSQL/operator status without credentials.

## Launcher protocol and recovery

- [ ] An A/B synthetic remote smoke completes through API, PostgreSQL, Bucket,
      presigned URL, download, hash verification, reconstruct, and install.
- [ ] The remote A-to-B update reproduces the expected approximate savings only
      after measuring it; output files are byte-identical.
- [ ] Interrupted download resumes with a Range request and 206 response, with
      captured offsets and final hash.
- [ ] A presigned URL expiry refreshes at chunk level through a new resolve.
- [ ] Bad secondary mirror then HOT and HOT failure then bad secondary both
      exercise retry/fallback without corrupting the cache.
- [ ] Cold-only resolution returns restore_pending and Retry-After.
- [ ] Worker reads MEGA, verifies BLAKE3, writes HOT, records the restored
      location, and the launcher downloads the restored object without seeing
      MEGA credentials.
- [ ] Other chunks continue while one chunk is restore_pending; retry/backoff
      eventually completes the install.

## Reliability and operations

- [ ] API, worker, and PostgreSQL restart/reconnect tests pass.
- [ ] Resource measurements cover API, worker, PostgreSQL, bucket traffic,
      MEGAcmd, memory, CPU, network, volume, and bounded temporary storage.
- [ ] Normal launcher concurrency does not create an unbounded worker,
      PostgreSQL, bucket, or temporary-file explosion.
- [ ] Signing uses explicit key ID staging-2026-01, with the private key only
      in staging secret storage and the public key in the staging launcher
      trust configuration.
- [ ] Production trust configuration does not contain the staging key or
      staging endpoint override.
- [ ] The evidence and latency results are recorded in
      docs/staging-performance.md.

## Operator commands

Read-only checks:

    launcher-admin db status
    launcher-admin storage policy
    launcher-admin storage health
    launcher-admin storage accounts list
    launcher-admin staging verify --api-url $env:LAUNCHER_STAGING_API_URL --require-cold

The last command checks liveness, readiness, redacted storage status, metrics,
policy, HOT/COLD availability, and optional manifest/signature trust. It does
not publish, restore, delete, or mutate bucket contents.

## Evidence packet

Attach only redacted evidence: deployment IDs, commit, timestamps, status
codes, byte counters, hashes, latency summaries, resource graphs, and
operator-approved MEGA smoke output. Omit URLs with query strings, all
credentials, session paths if sensitive, private keys, and account email
addresses.
