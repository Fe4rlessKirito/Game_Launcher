# Live Mantle staging validation

Validation date: 2026-08-13. This checklist records the current Mantle
deployment, not the retired Railway setup.

## PASS

- API, Caddy, PostgreSQL, Telegram Bot API, and private restore worker are up.
- `/v1/health` and `/v1/ready` return HTTP 200.
- `launcher-admin staging verify --require-cold` passes liveness, readiness,
  storage status, metrics, policy, and staging signature checks.
- HOT is FileMirage and COLD is Telegram; the API reports both healthy and
  the policy requires one verified copy in each tier.
- Real Telegram 512 MiB physical-pack upload, download, BLAKE3 verification,
  concurrency measurements, and smoke-object deletion pass.
- Current build B downloads directly from FileMirage; historical build A is
  streamed Telegram -> private worker -> API -> launcher.
- Remote synthetic A install and A -> B update pass with byte identity.
- The remote E2E runner also supports `-SkipLaunch` for an authorized real
  game directory; it still performs signed-manifest install, update, damage,
  repair, and full byte-identity checks, but does not attempt to launch the
  commercial executable.
- Deliberate HOT reference eviction followed by Telegram -> worker ->
  FileMirage restore passes BLAKE3/read-back verification.
- API and worker restart/reconnect checks recover with health/readiness 200 and
  zero pending pack restores.
- No remote provider deletion is used for the recovery test. Old HOT links are
  retired from Vaultnode and their provider-side objects are left to natural
  provider expiry.

## NOT ENABLED

- Buzzheavier: upload was observed, but direct download/range/resume were not.
- GoFile: free-tier direct HOT download was not observed.
- MEGA: not part of the current plan.
- Presigned URL expiry/refresh: not applicable to FileMirage's observed direct
  URLs, which had no expiry.

## Remaining limitations

- The remote run is an authorized synthetic game fixture, not a commercial
  game archive.
- A full 16-provider/real-client network benchmark is not implied by the
  small provider probes.
- The local no-database E2E helper still assumes the legacy logical resolver;
  the Mantle pack-canonical runner is the authoritative remote test.

See `docs/staging-performance.md` and `docs/provider-capability-records.md`
for the measured values and capability decisions.
