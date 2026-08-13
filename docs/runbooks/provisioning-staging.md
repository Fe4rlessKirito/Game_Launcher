# Staging provisioning runbook

This is a controlled staging procedure for the Mantle VPS. It does not claim
that Mantle, Cloudflare, or MEGA is currently connected.

1. Apply the forward migration with `launcher-admin db migrate`; verify
   `launcher-admin db status` reports the provisioning tables.
2. Configure `PROVISIONING_ENABLED=true`, the non-secret domain
   `vaultnode.pp.ua`, a random HMAC secret in the Mantle deployment, the same secret in the
   Cloudflare Worker, and the size/skew/alias TTL settings. Keep the secret
   out of Git, chat, logs, and email.
3. Deploy the API and the private provisioning/restore worker from the same
   repository. Do not give the worker a public domain. Mount a small persistent
   volume only at the MEGAcmd session/state directory; keep chunk transfers in
   bounded temporary space.
4. Run `launcher-admin provisioning readiness`. For the controlled email-route
   smoke only, set `PROVISIONING_ENABLE_FAKE=true` temporarily and run
   `launcher-admin provisioning test-email-address` without an address; it
   creates a short-lived fake-provider job and prints a random alias without
   allocating real provider capacity. Pass an existing alias to
   `launcher-admin provisioning test-email-address <alias>` to validate it
   without creating a job. Disable the fake provider after the smoke.
   `readiness` treats manual mode as valid; an absent automatic provider is
   not itself a failure.
5. For MEGA, use one operator-enrolled account with the existing
   `launcher-admin storage accounts add` command. Do not automate signup or
   send credentials to the API. Complete the corresponding job with
   `launcher-admin provisioning complete-manual`; the worker re-runs health,
   capacity, tiny random upload/download/BLAKE3/delete, and only then marks the
   candidate enrolled.
6. Exercise fake automatic provisioning in CI with
   `PROVISIONING_ENABLE_FAKE=true`; never use the fake provider for production
   capacity.

Record these checks for a real provider separately; this repository's automated
suite does not claim live MEGA, Cloudflare, or Mantle validation:

```text
MEGA network:     PASS
Authentication:   PASS
Upload:           PASS
Download:         PASS
Integrity:        PASS
Delete:           PASS
Cold pool:        READY
```

After a successful operator enrollment, run the existing storage restore smoke
and the remote staging suite. Destructive HOT deletion is allowed only for an
explicit staging build and operator confirmation.
