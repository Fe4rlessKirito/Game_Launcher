# Cloudflare email ingest

`infra/cloudflare/email-worker` is a generic TypeScript Email Worker. Configure
`PROVISIONING_INGEST_URL` to the API's private/internal endpoint and store
`INGEST_HMAC_SECRET` with `wrangler secret put`. Route only the
`vaultnode.pp.ua` provisioning aliases to this worker. No public API domain is
created for the worker.

The worker reads `message.raw`, computes SHA-256, signs this exact payload, and
POSTs the unchanged bytes as `message/rfc822`:

```text
timestamp\nnonce\nsha256(raw MIME)\nenvelope-from-or-empty\nenvelope-to
```

The Rust API verifies the signature before parsing MIME. It extracts
Message-ID, From, To, Subject, Date, `text/plain`, and `text/html`, then calls
the parser registered for the job's provider. The Message-ID ledger makes a
redelivery harmless. The worker can reject a non-2xx response; it never retries
by changing the body or forwarding a message unless the explicit debug flag is
enabled.

Run the local vector test with `npm test` from the worker directory. The test
uses no Cloudflare account or secret.
