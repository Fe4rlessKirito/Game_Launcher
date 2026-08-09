# Deployment

`deploy/compose.yaml` starts PostgreSQL, the API, and Caddy. Copy `deploy/env.example` into the staging secret-management workflow; do not commit a populated environment file. Run migrations explicitly before starting production traffic. PostgreSQL backups should use `pg_dump --format=custom` and be restored into a disposable database before a release is trusted.

## Environments

Development defaults to `LAUNCHER_STORAGE_PROVIDERS=local` and serves objects through the API's local proxy. Staging should use the same API image with PostgreSQL, HTTPS through Caddy, and at least one independently operated object location:

```text
launcher-admin publish
        |
        v
  PostgreSQL <- Axum API <- Caddy/TLS <- staging launcher
        |
        +--> local mirror (optional)
        +--> S3-compatible bucket
```

Set `LAUNCHER_STORAGE_PROVIDERS=local,s3` to exercise two providers in one staging publish, or `s3` when the staging bucket is the only byte store. `LAUNCHER_MIRROR_BASE_URLS` adds externally operated `/objects/{hash}` mirrors to resolver output. The API returns provider URLs first, then verified database locations, then configured static mirrors, with duplicates removed.

For S3-compatible storage, set the endpoint, region, bucket, access key, and secret key. Set `LAUNCHER_S3_PUBLIC_BASE_URL` only when the bucket or CDN exposes stable object URLs; otherwise the API generates short-lived presigned GET URLs. Keep the bucket private when using presigning. The API never proxies S3 bytes.

The publisher uploads content-addressed objects under `chunks/encoded/{blake3}.bin`, verifies the returned object by size, metadata, and a downloaded BLAKE3 hash, and records only stable URLs in `storage_locations`. Presigned locations are resolved at request time. Schedule `S3CompatibleStorage::cleanup_orphaned_multipart_uploads` from an operator/maintenance job with credentials scoped to the staging bucket; the configured age threshold prevents fresh uploads from being aborted.

## Staging checklist

1. Provision a dedicated bucket and database; grant the publisher/API only the required bucket and object-prefix permissions.
2. Inject secrets through the VPS secret store or deployment manager. Never put S3 or signing secrets in Compose, Git, launcher configuration, or client binaries.
3. Configure a real DNS name, Caddy email, and `LAUNCHER_PUBLIC_BASE_URL=https://...`; verify `/health` reports every configured provider as healthy.
4. Run the migration, publish an authorized test build with `DATABASE_URL` and the provider configuration, and verify the catalog, manifest signature, presigned/stable URLs, and direct chunk downloads.
5. Run the A→B→repair workflow over HTTPS with an interrupted download and an unavailable mirror. Check that the launcher retries another URL and that no API request carries chunk bytes.
6. Exercise VPS restart, PostgreSQL restore, bucket lifecycle cleanup, key rotation, and rollback before calling the environment production-ready.

The repository does not contain a VPS hostname, DNS zone, bucket, or production credentials, so staging deployment and real HTTPS validation remain operator actions.

## Railway API deployment

The root `railway.toml` points Railway at `deploy/api.Dockerfile`, starts
`/usr/local/bin/launcher-api`, and uses `/health` for health checks with an
on-failure restart policy. Create a Railway PostgreSQL plugin and inject its
`DATABASE_URL` into the API service. Set `LAUNCHER_AUTO_MIGRATE=1` for a
controlled first boot or run the migration through an operator job before
traffic is enabled.

Run the Astro website as a separate Railway service rooted at `website/`; the
API service does not serve the website bundle. Railway provides TLS and public
service domains, so Caddy is not part of the Railway topology. Keep the
existing Caddy Compose topology for a VPS deployment.

For Railway staging, configure at least one independently operated hot
provider and a cold MEGA pool, set the hot/cold policy variables, and provide
`LAUNCHER_MEGA_ACCOUNTS_FILE` through a protected file/volume mechanism. The
MEGAcmd sessions must be pre-authenticated by the operator; no Railway build or
startup step creates accounts or handles passwords.
