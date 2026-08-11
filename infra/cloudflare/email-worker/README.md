# Cloudflare provisioning email worker

This worker receives Cloudflare Email Routing messages for the random aliases
created by the provisioning service. It forwards the raw `message/rfc822` body
to the private API endpoint and signs the request with HMAC-SHA256.

The signed canonical payload is exactly five newline-delimited fields:

```text
timestamp
nonce
sha256(raw MIME)
envelope-from-or-empty
envelope-to
```

Set the API URL and the same HMAC secret on the worker. Keep the secret in a
Cloudflare Worker secret, never in this repository or an email body. Debug
forwarding is disabled by default and is only allowed when both
`DEBUG_FORWARD_ENABLED` and `DEBUG_FORWARD_ADDRESS` are explicitly set.

The worker has no signup or provider-specific logic. The API owns MIME parsing,
alias expiry, replay protection, provider parsing, validation, and enrollment.
