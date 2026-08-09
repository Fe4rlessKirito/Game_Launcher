# Security model

The platform assumes authorized content and does not attempt to defeat DRM or access controls. Threats considered in v1 include malicious manifests, corrupted providers, partial downloads, path traversal, stale URLs, compromised local cache entries, and interrupted installation.

The highest-risk future work is production key-ring management, updater package signing, Windows single-instance IPC hardening, and provider credential isolation. Local signatures, updater hashes, and path/reparse-point checks are validation-phase controls; they are not a substitute for production key custody or a security review.

S3 credentials are server-side configuration only. The API exposes either stable public object URLs or scoped presigned GET URLs; clients never receive access keys. Presigned URLs are not persisted in database mirror records because they expire. Staging and production should use separate buckets/credentials, private buckets when presigning, prefix-scoped IAM policies, TLS, short URL lifetimes, and lifecycle cleanup for incomplete multipart uploads.

The current `launcher-admin publish` path still supports embedded local fixture keys. Before public release, replace that mode with a controlled signing operation backed by an encrypted key store or external signing service, retain overlapping trusted public keys during rotation, and require an explicit approval step before `PUBLISHED`.

Cold storage adds an operator-only boundary. MEGA account configuration stores
credential references and isolated MEGAcmd session directories, not raw
passwords. Enrollment reuses a pre-authenticated session and does not automate
signup, CAPTCHA, or password recovery. The API filters cold providers from
download resolution; restore workers verify BLAKE3 before recording a hot
replica. Capacity, health, and restore status endpoints redact session material
and secrets.
