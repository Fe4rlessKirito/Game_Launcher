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

FileMirage remains probe-only until it publishes a usable API contract. No
browser automation or Playwright path is permitted for storage providers.
