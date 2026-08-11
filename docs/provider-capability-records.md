# Provider capability records

Validation run: 2026-08-11. These are observed results from small, isolated
operator-authorized probes. They are not provider guarantees, and they do not
enable a provider. Only `local` and the configured S3-compatible HOT provider
are currently accepted by the storage registry; FileMirage, Buzzheavier, and
GoFile remain out of `LAUNCHER_STORAGE_PROVIDERS`.

## FileMirage

| Capability | Observed result |
| --- | --- |
| `upload_api` | `true` — public client chunk upload returned success for a small physical pack. |
| `direct_download` | `true` — resolved direct URL downloaded the exact source bytes. |
| `range` | `true` — `206` with `Content-Range` and `Accept-Ranges: bytes`. |
| `resume` | `true` — two-part byte-range download matched the source exactly. |
| `stable_url` | `false` — not proven; repeated page fetches produced different direct backend URLs. |
| `cookies_required` | `false` for the observed direct GET. |
| `HEAD` | `false` — the resolved direct path returned `405`. |
| `delete` | `false` — no authenticated delete contract was observed. |
| `observed_speed` | Approximately `0.15–0.20 MiB/s` aggregate for a `865 KiB` object at 4/8/16 concurrent GETs; too small for sizing. |
| `safe_concurrency` | `unproven`; 4, 8, and 16 completed byte-for-byte, but this is not a production limit. |

## Buzzheavier

| Capability | Observed result |
| --- | --- |
| `upload_api` | `true` — anonymous `PUT` returned `201` for a small object. |
| `direct_download` | `false`/unproven — the public download route was not a usable direct object response in the probe. |
| `range` | unproven |
| `resume` | unproven |
| `stable_url` | unproven |
| `cookies_required` | unproven |
| `HEAD` | unproven |
| `delete` | `false`/unproven — the anonymous test object could not be removed through the observed unauthenticated API. |
| `observed_speed` | unmeasured |
| `safe_concurrency` | unproven |

## GoFile free tier

| Capability | Observed result |
| --- | --- |
| `upload_api` | `true` — anonymous upload returned a guest content record. |
| `direct_download` | `false` — free guest direct-link creation returned `401`; the documented direct-link path is Premium. |
| `range` | unproven |
| `resume` | unproven |
| `stable_url` | `false` for HOT use — only a download page was available in the free probe. |
| `cookies_required` | unproven |
| `HEAD` | unproven |
| `delete` | `true` — the guest content record was deleted through the guest-token API. |
| `observed_speed` | unmeasured |
| `safe_concurrency` | unproven |

## Telegram COLD

The 512 MiB real-account probe is not PASS yet. No bot token, chat ID, or
private Local Bot API endpoint is present in this environment. The public
[Telegram Bot API](https://core.telegram.org/bots/api) is limited to 50 MB
bot uploads and 20 MB `getFile` downloads, so the requested 512 MiB test must
run through the official [Local Bot API Server](https://github.com/tdlib/telegram-bot-api)
in private `--local` mode. The fake-provider suite passes upload, restore,
BLAKE3 verification, and delete, but it is not a real Telegram network test.

Until the real run is completed, the only honest status is:

```text
Telegram network:  NOT RUN
Authentication:    NOT RUN
Upload:            NOT RUN
Download:          NOT RUN
Integrity:         NOT RUN
Delete:            NOT RUN
Cold pool:         NOT READY
```

The provider is therefore not enabled for the 512 MiB staging gate.
