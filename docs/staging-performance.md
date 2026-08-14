# Staging performance record

Validation run: 2026-08-13 on Mantle staging. These are observed staging
measurements, not production capacity promises.

## Environment

- API and restore worker: Mantle VPS, private Docker network
- PostgreSQL: Mantle Docker service
- HOT: FileMirage direct URLs
- COLD: Telegram through the private Bot API service and restore worker
- physical-pack target/max: 512 MiB
- logical HOT replicas: 1
- required COLD replicas: 1
- restore mode: ON_DEMAND

## Telegram 512 MiB smoke

The requested 512 MiB probe used a 534,786,242-byte pack and verified the
same BLAKE3 digest after download.

| Concurrency | Elapsed | Throughput |
| ---: | ---: | ---: |
| 1 | 503 ms | 1013.35 MiB/s |
| 2 | 591 ms | 1723.61 MiB/s |
| 4 | 914 ms | 2231.49 MiB/s |
| 8 | 1810 ms | 2253.27 MiB/s |
| 16 | 3609 ms | 2260.68 MiB/s |

These numbers include the local Bot API path on the Mantle host and are not
internet-client throughput measurements.

## Remote synthetic A to B

| Phase | Result |
| --- | --- |
| A install | PASS; 8,478,699 network bytes; physical-pack amplification 1.000401x |
| B update | PASS; 9,527,646 network bytes; physical-pack amplification 1.000377x |
| byte identity | PASS |
| current-build data plane | PASS; direct host was `filemirage.com` |
| historical-build data plane | PASS; source was the private API cold-stream route |

The remote runner also reported `reused_installed_bytes=4,357,397` and
`reconstructed_bytes=5,261,688` during A to B. Its network-savings field is
zero in pack-canonical mode because the physical pack is the measured unit;
the amplification values above are the relevant traffic metric.

## Cold to HOT recovery

Build `staging-b` pack
`8f3485af29067447da1339e080d0ac563b790084bc170377f62ebd4b6ff0ab71`:

```text
HOT_REFERENCE_EVICTED: PASS (remote_delete=NOT_RUN)
Telegram COLD source:  PASS
BLAKE3 verification:  PASS
FileMirage re-upload: PASS
HOT read-back verify: PASS
restore job 33:       DONE
```

The worker now retries and verifies the HOT read-back before completing a
pack restore, so a transient provider 500 cannot be recorded as success.

## Restart and readiness

```text
/v1/health: PASS 200
/v1/ready:  PASS 200
storage status: PASS 200; HOT healthy=1; COLD healthy=1
staging verify: PASS
worker after restart: RUNNING; cold stream LISTENING; pending pack restores=0
```

## Not applicable / remaining provider gates

- Presigned URL expiry/refresh is not applicable to the active FileMirage
  direct-URL capability record: `expires_at` is null and no presigned URL was
  observed. The presign-specific test is intentionally not reported as a
  pass.
- Buzzheavier upload succeeded, but direct download and range/resume were not
  proven, so it remains disabled.
- The isolated local no-database E2E harness was not used as staging evidence;
  its legacy pack resolver returns 503 when physical-pack storage is disabled.
- The authorized Steam Spacewar real-game install/update/repair run is recorded
  in `docs/staging-validation.md`; this performance record intentionally keeps
  the larger throughput table focused on synthetic A/B measurements.
