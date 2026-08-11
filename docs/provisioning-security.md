# Provisioning security boundary

- PostgreSQL stores job state, hashes, provider/account references, and secret
  references. It never stores MEGA passwords, provider tokens, raw MIME, or
  session material.
- `SecretRef` values point to a `SecretStore`; the repository includes memory
  and filesystem implementations. Debug and display implementations redact
  material. Operator output reports `credential_configured`, not the value.
- Cloudflare requests carry a timestamp, random nonce, SHA-256 of the exact raw
  MIME body, envelope sender/recipient, and HMAC-SHA256 signature. The server
  verifies clock skew, body binding, recipient binding, and the signature with
  a constant-time MAC comparison. Nonces are claimed once in PostgreSQL.
- The API reads at most `PROVISIONING_EMAIL_MAX_BYTES + 1` bytes and returns
  413 for an oversized message. Invalid signatures, timestamps, recipients,
  and replays do not disclose whether an alias exists.
- Random aliases are job-scoped, expire with the job, and are invalidated on
  enrollment, cancellation, permanent failure, or timeout. The database keeps
  only the alias token hash in the job record.
- The Cloudflare worker has no provider signup logic and does not log raw
  messages, signatures, or secrets. Debug forwarding is disabled by default.
- MEGA signup, password entry, CAPTCHA, recovery, and account enrollment stay
  operator-controlled. Automatic providers must return a candidate that the
  server validates before enrollment.
