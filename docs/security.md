# Security model

The platform assumes authorized content and does not attempt to defeat DRM or access controls. Threats considered in v1 include malicious manifests, corrupted providers, partial downloads, path traversal, stale URLs, compromised local cache entries, and interrupted installation.

The highest-risk future work is production key-ring management, updater package signing, Windows single-instance IPC hardening, and provider credential isolation. Local signatures, updater hashes, and path/reparse-point checks are validation-phase controls; they are not a substitute for production key custody or a security review.
