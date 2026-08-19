# Live Mantle staging validation

Validation date: 2026-08-15. This checklist records the current Mantle
deployment and its FileMirage HOT/Telegram COLD topology.

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
- The live physical-pack recovery used `staging-b`, pack
  `4d5dcc2cb0126045a40f8620e29bc9410f0cdea7e040a4edbf9e1634ded37251`,
  and completed as restore job `34` with 8,478,699 verified bytes.
- API and worker restart/reconnect checks recover with health/readiness 200 and
  zero pending pack restores.
- `vaultnode.pp.ua` now resolves to the Mantle VPS; Caddy serves a valid
  Let's Encrypt certificate, redirects HTTP to HTTPS, and authenticated
  metrics pass through the public hostname.
- PostgreSQL local-only backup mode is enabled for the no-user staging
  environment; the daily timer is active and the last local dump/checksum
  completed successfully.
- Workstation archive-normalization probe passes ZIP, TAR, 7z, and authorized
  RAR inputs through `launcher-admin ingest`; each reaches `stage=Ready` after
  bounded extraction and cleanup.
- The Avalonia client was launched against `https://vaultnode.pp.ua`, hydrated
  the live catalog, and showed the persisted download history and live
  install-state badges. Historical settings pointing at the Mantle IP are
  automatically migrated and persisted to the HTTPS hostname. The previously
  validated authorized-Spacewar UI install and repair flow remains covered by
  the 2026-08-14 run below.
- No remote provider deletion is used for the recovery test. Old HOT links are
  retired from Vaultnode and their provider-side objects are left to natural
  provider expiry.

## Authorized real-game Mantle run

On 2026-08-14, the installed Steam Spacewar sample was copied into disposable
ignored A/B fixtures; the original installation was not modified. Both builds
were ingested, signed, and published to Mantle with FileMirage HOT and
Telegram physical-pack COLD placement. The launcher then ran the real
install -> update -> damage -> repair flow against the Mantle API. Launch was
explicitly skipped because this is a storage/install validation, not an
attempt to start the user's game executable.

Observed results:

| Phase | Result |
| --- | --- |
| A install | 8 files; 905,795 logical encoded bytes; BLAKE3 byte identity PASS |
| A -> B update | 9 files; 1,904,486 installed bytes reused; 1,642 bytes reconstructed |
| B repair after corruption/removal | PASS; 0 network bytes; BLAKE3 byte identity PASS |
| Data plane | PASS; resolved direct host was `filemirage.com`, not the API |
| Pack amplification | A: `1.001x`; B update: `1.999x` physical traffic/logical bytes |

The B update amplification is expected in canonical physical-pack mode: the
launcher downloads the complete pack containing the changed logical data.
This is the recorded baseline for future pack-size/provider tuning.

## NOT ENABLED

- Buzzheavier: upload was observed, but direct download/range/resume were not.
- GoFile: free-tier direct HOT download was not observed.
- MEGA: not part of the current plan.
- Presigned URL expiry/refresh: not applicable to FileMirage's observed direct
  URLs, which had no expiry.

## Remaining limitations

- The remote real-game run validates authorized Steam Spacewar bytes but does
  not launch the executable; the synthetic run remains the launch-validation
  fixture.
- A full 16-provider/real-client network benchmark is not implied by the
  small provider probes.
- The local no-database E2E helper still assumes the legacy logical resolver;
  the Mantle pack-canonical runner is the authoritative remote test.
- The desktop client uses a ten-minute streaming request timeout for manifest,
  pack, and restore transfers; catalog refresh still has its own short
  connectivity timeout.
- Commit `db74890` was rebuilt and deployed to Mantle on 2026-08-14. The live
  API reports trusted proxy headers enabled; unauthenticated storage-admin
  access returns HTTP 401 while `/v1/health` and `/v1/ready` return HTTP 200.
- Mantle now mounts the operator-managed `mantle-2026-08-14` signing key
  read-only into the worker. A live `launcher-admin manifest-sign` probe
  succeeded without exposing the private key, and the production launcher key
  ring matches the public keys on the existing published builds.
- Off-host backup replication is intentionally not enabled for this no-user
  staging deployment. The production preflight remains fail-closed until a
  separate backup destination is configured.

See `docs/staging-performance.md` and `docs/provider-capability-records.md`
for the measured values and capability decisions.
