# MEGA cold storage

The optional MEGA adapter uses the maintained official [MEGAcmd command-line
client](https://github.com/meganz/MEGAcmd) through a bounded subprocess boundary.
The official [MEGAcmd user guide](https://github.com/meganz/MEGAcmd/blob/master/UserGuide.md)
documents the scriptable `mega-whoami`, `mega-df`, `mega-du`, `mega-put`,
`mega-get`, and `mega-rm` commands used by the adapter. The native
[MEGA C++ SDK](https://github.com/meganz/sdk) remains a possible future adapter,
but is intentionally not pulled into the Rust server build.

When MEGA is explicitly enabled, its worker image must pin the official Debian
12 amd64 MEGAcmd package
`2.5.2-1.1` by SHA-256. The package is installed in the image; only the
authenticated MEGAcmd home is persisted on the Railway volume.

## Account configuration

`LAUNCHER_MEGA_ACCOUNTS_FILE` points to an operator-managed JSON file. It
contains account IDs, remote roots, capacity hints, and a
`credential_reference`; it does not contain a MEGA password. A representative
shape is:

```json
{
  "provider_id": "mega-cold",
  "tier": "COLD",
  "reservation_ttl_seconds": 3600,
  "verify_existing": true,
  "accounts": [
    {
      "account_id": "mega-a",
      "credential_reference": "secret://mega/a/session",
      "home_dir": "/var/lib/launcher/megacmd/a",
      "remote_root": "/launcher",
      "capacity_bytes": 0,
      "safety_margin_bytes": 10737418240
    }
  ]
}
```

Each account gets an isolated MEGAcmd home directory. The operator must
pre-authenticate that session and provision its filesystem permissions. The
launcher never automates signup, CAPTCHA, password collection, or account
recovery. `launcher-admin storage accounts add` accepts only a credential
reference and session paths, verifies the existing session, and records health
and capacity status.

## Object layout and safety

The layout is deterministic and content addressed:

```text
<remote_root>/chunks/<first-two-hash-bytes>/<next-two-hash-bytes>/<hash>.chunk
```

Uploads are idempotent. An existing object is accepted only when its size
matches and, when `verify_existing` is enabled, a downloaded BLAKE3 check also
matches. New uploads are size checked after transfer. Reads verify size and
BLAKE3 before returning bytes.

The pool sorts accounts by ID, refreshes capacity, reserves bytes in PostgreSQL
under a row lock, and rolls to the next account when the current account is
full, near its configured safety margin, unavailable, or awaiting reauth.
The safety margin is configuration, not a fixed 20 GB rule. Pool status is
reported as `READY`, `DEGRADED`, `NEEDS_CAPACITY`, or `UNAVAILABLE`.

## Operations

```powershell
launcher-admin storage accounts add --account-id mega-a `
  --credential-reference secret://mega/a/session `
  --home-dir C:\launcher\megacmd\a --provider-id mega-cold
launcher-admin storage accounts list --provider-id mega-cold
launcher-admin storage health --storage-root C:\launcher\storage
launcher-admin storage restore-pending
```

Run the fake-provider tests in CI. MEGA is not part of the Telegram staging
gate. If it is enabled later, a real MEGA smoke test must be explicitly
credential-gated and must use an operator-provided pre-authenticated session;
it is never a default CI or deployment step.
