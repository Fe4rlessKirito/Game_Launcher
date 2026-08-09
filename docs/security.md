# Security model

The platform assumes authorized content and does not attempt to defeat DRM or access controls. Threats considered in v1 include malicious manifests, corrupted providers, partial downloads, path traversal, stale URLs, compromised local cache entries, and interrupted installation.

The highest-risk future work is key management, updater rollback, Windows single-instance IPC, junction/symlink hardening, and provider credential isolation. Each must have adversarial tests before production publication.
