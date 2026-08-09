# Deployment

`deploy/compose.yaml` starts PostgreSQL, the API, and Caddy. Copy `deploy/env.example` to an environment-specific secret store; do not commit credentials. Run migrations explicitly before starting production traffic. PostgreSQL backups should use `pg_dump --format=custom` and be restored into a disposable database before a release is trusted.

The API is intended to sit behind TLS termination and a CDN or object-store edge. Chunk bytes should be served by a storage provider, not streamed through the API container. Rotate signing keys with overlapping verification windows and retain the old public key until all supported clients have updated.
