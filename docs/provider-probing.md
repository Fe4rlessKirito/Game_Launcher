# Provider probing

`launcher-admin storage probe --provider <id>` prints capability metadata and
performs no network request by default. Add `--live` only for an operator-
authorized staging check. The live probe checks provider health and reports a
structured failure without printing credentials, signed URLs, bot tokens, or
session material.

Provider readiness must establish upload, read, delete, integrity verification,
direct-link behavior, range behavior, URL expiry/refresh, and safe concurrent
request limits. A provider can be healthy for COLD restore while still being
ineligible for direct HOT downloads.

The official HTTP references used for adapter decisions are:

- [Buzzheavier Developers](https://buzzheavier.com/developers) and
  [Buzzheavier help](https://buzzheavier.com/help);
- [GoFile API](https://gofile.io/api);
- [Telegram Bot API](https://core.telegram.org/bots/api).

FileMirage is the active HOT provider after the recorded staging probe proved
direct reads, byte ranges, resume, and integrity on the upload API's returned
direct URL. Its URLs are still treated as non-stable and are renewed before
the provider's inactivity window. No browser automation or Playwright path is
permitted for storage providers.

Buzzheavier remains upload-only until an authenticated or provider-supported
machine-download path is proven. An anonymous 2026-08-14 probe returned HTTP
201 for upload, but the returned download paths were intercepted by a
Cloudflare challenge (HTTP 403), so direct download, range/resume, and cleanup
are not enabled. Do not set the Buzzheavier capability flags to `true` merely
because upload succeeds.
