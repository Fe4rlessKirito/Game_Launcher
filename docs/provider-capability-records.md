# Provider capability records

Validation run: 2026-08-13 on the Mantle staging deployment. These are
observed results from operator-authorized probes, not provider guarantees.
Only capabilities marked `true` are used by the runtime resolver.

## FileMirage HOT

| Capability | Observed result |
| --- | --- |
| `upload_api` | `true` - physical-pack PUT succeeded. |
| `direct_download` | `true` - direct URL returned the exact source bytes. |
| `range` | `true` - 206 with Content-Range was observed. |
| `resume` | `true` - a two-part byte-range download matched the source. |
| `stable_url` | `false` - not proven; renewed uploads produced new URLs. |
| `cookies_required` | `false` for the observed direct GET. |
| `HEAD` | `false` - the direct path returned 405. |
| `delete` | `false` - no authenticated delete contract was observed. |
| `observed_speed` | Measured on small objects only; not a pack-sizing guarantee. |
| `safe_concurrency` | `unproven`; 4, 8, and 16 completed byte-for-byte in the small probe. |

FileMirage is the active HOT provider. Its URLs are used directly by the
launcher; the API does not proxy current-build HOT bytes.

## Buzzheavier HOT candidate

| Capability | Observed result |
| --- | --- |
| `upload_api` | `true` - anonymous PUT returned 201 for a small object. |
| `direct_download` | `false`/unproven - no usable direct object response was observed. |
| `range` | unproven |
| `resume` | unproven |
| `stable_url` | unproven |
| `cookies_required` | unproven |
| `HEAD` | unproven |
| `delete` | `false`/unproven |
| `observed_speed` | unmeasured |
| `safe_concurrency` | unproven |

Buzzheavier remains disabled for HOT reads. Upload-only behavior is not
enough to expose it to the launcher.

## GoFile free tier

The earlier free-tier probe observed upload and guest-token deletion, but no
usable direct HOT download link. GoFile is not enabled.

## Telegram COLD

The real 512 MiB pack smoke ran through the private Telegram Bot API service:

```text
network:   PASS
authentication: PASS
upload:    PASS
download:  PASS
integrity: PASS
delete:    PASS (smoke object only)
cold pool: READY
```

The smoke object was deleted after verification. Real game packs are not
deleted by routine retention or recovery operations. When a build becomes
historical, its HOT reference is retired from Vaultnode while the last
provider copy is left to expire naturally; Telegram remains the retained COLD
copy.
