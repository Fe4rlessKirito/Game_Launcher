# Launcher staging setup for the operator

This is the operator runbook for the `Launcher-Staging` Railway environment.
It assumes the repository commits have been pushed to the GitHub branch that
Railway will deploy. It never asks for passwords, tokens, S3 secret values, or
private signing keys in chat.

Use the Railway dashboard for project ownership and secret entry. The CLI is
only recommended for commands inside an already deployed worker container.
Railway's current references are:

- Config as Code: https://docs.railway.com/config-as-code
- Variables: https://docs.railway.com/variables
- Storage Buckets: https://docs.railway.com/storage-buckets
- Volumes: https://docs.railway.com/volumes
- Private networking: https://docs.railway.com/networking/private-networking
- `railway ssh`: https://docs.railway.com/cli/ssh

## Service names and repository paths

Use these service names so the variable references below work without editing:

| Resource | Name | Public? |
| --- | --- | --- |
| PostgreSQL | `Postgres` | No |
| Bucket | `HotBucket` | No |
| API | `launcher-api` | Yes, HTTPS only |
| Restore worker | `launcher-restore-worker` | No |

The repository root is `/`. The API config is `/railway.toml`. The worker
uses the custom config file `/railway.worker.toml`, selected in that service's
Settings page. The API Dockerfile is `deploy/api.Dockerfile`; the worker
Dockerfile is `deploy/worker.Dockerfile`.

## 1. Prepare local staging material

From the repository root on Windows:

~~~
.\scripts\staging\setup-staging.ps1
~~~

Success looks like:

~~~
staging_setup=READY
staging_key_id=staging-2026-01
synthetic_fixture=...\artifacts\staging-fixture
~~~

The command creates a private key and a public key under
`artifacts\staging-keys`. The private file is ignored by Git. Keep it locally
until it is entered directly into the selected Railway secret field; never
paste it into this chat or commit it.

If this fails, send only the command name, exit code, and the final
non-secret error line.

## 2. Create the Railway project

In Railway:

1. Open the workspace that owns the deployment.
2. Select `New Project`.
3. Select `Empty Project`.
4. Name the project `Launcher-Staging`.
5. Create or select a staging environment named `staging`.

Success: the project canvas shows an empty `staging` environment.

If the workspace or environment is unavailable, report the visible
workspace/environment name. Do not send an account token.

## 3. Add PostgreSQL

In the `Launcher-Staging` project:

1. Select `+ New`.
2. Select `Database`.
3. Select `PostgreSQL`.
4. Rename the service to `Postgres`.
5. Wait until its deployment is `Healthy`.

Success: the `Postgres` service exposes a `DATABASE_URL` variable in the
staging environment. Do not copy or send its value.

If PostgreSQL is not healthy, send the service status and deployment log
summary, with connection strings redacted.

## 4. Add the private HOT bucket

In the same project:

1. Select `+ New`.
2. Select `Storage`.
3. Select `Bucket`.
4. Rename it to `HotBucket`.
5. Leave it private. Do not create a public bucket.

Success: `HotBucket` exposes `ENDPOINT`, `REGION`, `BUCKET`,
`ACCESS_KEY_ID`, and `SECRET_ACCESS_KEY` variables.

If the Bucket option is not visible, report the Railway workspace plan and
region shown in the UI; do not substitute a public object store.

## 5. Create the API service from GitHub

1. Select `+ New`.
2. Select `GitHub Repo`.
3. Choose the launcher repository and the branch containing the two staging
   commits.
4. Name the service `launcher-api`.
5. Set Root Directory to `/`.
6. Leave the Config as Code path at `/railway.toml`.
7. Deploy once so Railway creates the service.
8. In `Networking`, select `Generate Domain`.

The checked-in API config supplies:

~~~
Dockerfile: deploy/api.Dockerfile
start: /usr/local/bin/launcher-api
bind: 0.0.0.0:$PORT
healthcheck: /v1/health
~~~

Success:

~~~
https://<generated-api-domain>/v1/health
~~~

returns HTTP 200 with JSON containing `status: "ok"`.

If deployment fails, send the build/deploy failure stage and the redacted
final error line. Do not send environment-variable values.

## 6. Configure API variables without copying secrets

Open `launcher-api -> Variables -> Raw Editor` and add these values. The
${{...}} entries are Railway service references and are resolved by Railway;
do not replace them with copied secrets.

~~~
DATABASE_URL=${{Postgres.DATABASE_URL}}
LAUNCHER_PUBLIC_BASE_URL=https://${{RAILWAY_PUBLIC_DOMAIN}}
LAUNCHER_STORAGE_PROVIDERS=s3
LAUNCHER_STORAGE_MIN_HOT_REPLICAS=1
LAUNCHER_STORAGE_MIN_COLD_REPLICAS=1
LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS=1
LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS=1
LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED=true
LAUNCHER_STORAGE_RESTORE_MODE=ON_DEMAND
LAUNCHER_RESTORE_TARGET_PROVIDER=railway-hot
LAUNCHER_S3_PROVIDER_ID=railway-hot
LAUNCHER_S3_TIER=HOT
LAUNCHER_S3_ENDPOINT=${{HotBucket.ENDPOINT}}
LAUNCHER_S3_REGION=${{HotBucket.REGION}}
LAUNCHER_S3_BUCKET=${{HotBucket.BUCKET}}
LAUNCHER_S3_ACCESS_KEY=${{HotBucket.ACCESS_KEY_ID}}
LAUNCHER_S3_SECRET_KEY=${{HotBucket.SECRET_ACCESS_KEY}}
LAUNCHER_S3_FORCE_PATH_STYLE=false
LAUNCHER_S3_PRESIGN_TTL_SECONDS=900
LAUNCHER_S3_MULTIPART_THRESHOLD_BYTES=8388608
LAUNCHER_S3_MULTIPART_PART_BYTES=8388608
LAUNCHER_S3_MAX_ATTEMPTS=4
LAUNCHER_S3_MAX_CONCURRENT_REQUESTS=4
LAUNCHER_S3_ORPHAN_MAX_AGE_SECONDS=86400
LAUNCHER_AUTO_MIGRATE=0
RUST_LOG=info
~~~

If Railway does not expose `RAILWAY_PUBLIC_DOMAIN` to the variable editor,
first generate the domain, copy only the public HTTPS hostname into
`LAUNCHER_PUBLIC_BASE_URL`, and keep the `https://` prefix. This is the only
non-secret API value that may be copied from the UI.

Success: the service shows the variables as staged and no unresolved
${{...}} references.

## 7. Create the restore worker service

1. Select `+ New -> GitHub Repo`.
2. Select the same repository and branch.
3. Name the service `launcher-restore-worker`.
4. Set Root Directory to `/`.
5. Open the service Settings page.
6. Set the custom Config as Code file to `/railway.worker.toml`.
7. Do not generate a public domain.
8. Add a Railway volume mounted at `/var/lib/launcher`.

The volume must remain attached to this service. It stores:

~~~
/var/lib/launcher/megacmd
/var/lib/launcher/storage
~~~

The entrypoint repairs volume ownership and runs the admin worker as the
unprivileged `launcher` user. Temporary MEGAcmd files use
`/tmp/launcher-mega` and are not treated as durable storage.

Success: the worker deployment shows no public domain and the volume mount is
`/var/lib/launcher`.

## 8. Install and verify the official MEGAcmd runtime

The checked-in worker image intentionally does not download an unpinned
third-party MEGAcmd artifact. Before enabling `mega`, use the official
MEGAcmd package or an operator-owned image built from the official MEGAcmd
source. Pin the version and image/package digest in the operator's image
registry. Official references:

- Packages and platform guidance: https://github.com/meganz/MEGAcmd
- Command and session guide: https://github.com/meganz/MEGAcmd/blob/master/UserGuide.md

The deployed worker must have `mega-whoami`, `mega-df`, `mega-du`,
`mega-put`, `mega-get`, and `mega-rm` on `PATH`. The entrypoint exits with
`diagnostic=MEGA_RUNTIME_MISSING` if `mega` is enabled and `mega-whoami` is
absent.

Success inside the worker shell:

~~~
command -v mega-whoami
command -v mega-put
~~~

If the approved MEGAcmd image cannot be built, stop before entering an
account session and report the pinned version, base image, and build error.

## 9. Configure worker variables

Add these variables to `launcher-restore-worker`:

~~~
DATABASE_URL=${{Postgres.DATABASE_URL}}
LAUNCHER_PUBLIC_BASE_URL=https://${{RAILWAY_PUBLIC_DOMAIN}}
LAUNCHER_STORAGE_ROOT=/var/lib/launcher/storage
LAUNCHER_STORAGE_PROVIDERS=s3,mega
LAUNCHER_STORAGE_MIN_HOT_REPLICAS=1
LAUNCHER_STORAGE_MIN_COLD_REPLICAS=1
LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS=1
LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS=1
LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED=true
LAUNCHER_STORAGE_RESTORE_MODE=ON_DEMAND
LAUNCHER_RESTORE_TARGET_PROVIDER=railway-hot
LAUNCHER_MEGA_ACCOUNTS_FILE=/var/lib/launcher/megacmd/mega-accounts.json
LAUNCHER_S3_PROVIDER_ID=railway-hot
LAUNCHER_S3_TIER=HOT
LAUNCHER_S3_ENDPOINT=${{HotBucket.ENDPOINT}}
LAUNCHER_S3_REGION=${{HotBucket.REGION}}
LAUNCHER_S3_BUCKET=${{HotBucket.BUCKET}}
LAUNCHER_S3_ACCESS_KEY=${{HotBucket.ACCESS_KEY_ID}}
LAUNCHER_S3_SECRET_KEY=${{HotBucket.SECRET_ACCESS_KEY}}
LAUNCHER_S3_FORCE_PATH_STYLE=false
LAUNCHER_S3_PRESIGN_TTL_SECONDS=900
LAUNCHER_S3_MULTIPART_THRESHOLD_BYTES=8388608
LAUNCHER_S3_MULTIPART_PART_BYTES=8388608
LAUNCHER_S3_MAX_ATTEMPTS=4
LAUNCHER_S3_MAX_CONCURRENT_REQUESTS=4
LAUNCHER_S3_ORPHAN_MAX_AGE_SECONDS=86400
LAUNCHER_AUTO_MIGRATE=0
RUST_LOG=info
~~~

Add `LAUNCHER_SIGNING_PRIVATE_KEY_PEM` only if a controlled Railway admin
job will sign staging manifests. It is a sealed Railway secret. The running
API does not need the private key. Never print it or send it back.

Success: all `HotBucket` and `Postgres` references resolve, and the worker
does not show an unresolved-variable error.

## 10. Run migrations and verify database state

Run the migration from the worker service so it uses Railway's private
`DATABASE_URL`:

~~~
railway ssh --service launcher-restore-worker -- sh -lc "exec gosu launcher /usr/local/bin/launcher-admin db migrate"
railway ssh --service launcher-restore-worker -- sh -lc "exec gosu launcher /usr/local/bin/launcher-admin db status"
~~~

Expected status output contains:

~~~
"database": "CONNECTED"
"schema_ready": true
~~~

The schema includes `games`, `builds`, `chunks`, `build_chunks`,
`storage_locations`, `storage_objects`, `storage_accounts`,
`storage_reservations`, `storage_health_events`, and `restore_jobs`.

If migration fails, send the migration command's final error line and the
service deployment status. Do not send `DATABASE_URL`.

## 11. Verify API readiness and HOT storage

From the repository root:

~~~
$env:LAUNCHER_STAGING_API_URL = "https://<generated-api-domain>"
.\scripts\staging\verify-staging.ps1 -RequireCold
~~~

For the first check, omit `-RequireCold` if MEGA has not yet been enrolled:

~~~
.\scripts\staging\verify-staging.ps1
~~~

The command checks `/v1/health`, `/v1/ready`, storage status, metrics, and
the configured policy. It does not publish, delete, restore, or print
presigned URLs.

Then run a temporary direct Bucket smoke from the worker:

~~~
railway ssh --service launcher-restore-worker -- sh -lc "export HOME=/var/lib/launcher/megacmd; exec gosu launcher /usr/local/bin/launcher-admin storage smoke --provider hot --storage-root /var/lib/launcher/storage"
~~~

Success includes:

~~~
check=HOT_put status=PASS
check=HOT_head status=PASS
check=HOT_get status=PASS
check=HOT_download_url status=PASS
check=HOT_delete status=PASS
storage_smoke=PASS
~~~

If this fails, report the check name and non-secret error. Do not copy the
presigned URL.

## 12. Enroll one MEGA account without exposing its password

The account owner supplies one existing permitted MEGA account. Do not create
an account through automation and do not pass a password as a command-line
argument.

Open a shell with the persistent MEGAcmd home:

~~~
railway ssh --service launcher-restore-worker -- sh -lc "export HOME=/var/lib/launcher/megacmd; exec gosu launcher mega-cmd"
~~~

At the MEGAcmd prompt, use the interactive `login` flow. Enter the password
only when the interactive client requests it. Then run `whoami` and exit the
MEGAcmd shell without printing session material.

Back in the worker shell, enroll exactly one account:

~~~
railway ssh --service launcher-restore-worker -- sh -lc "export HOME=/var/lib/launcher/megacmd; exec gosu launcher /usr/local/bin/launcher-admin storage accounts add --account-id mega-a --credential-reference secret://mega/a/session --home-dir /var/lib/launcher/megacmd --remote-root /launcher-staging --safety-margin-bytes 10737418240 --provider-id mega-cold"
~~~

Success is `status=ACTIVE` and a capacity value. A network problem is
reported as `diagnostic=MEGA_NETWORK_UNAVAILABLE`; a session problem is
reported as an authentication failure. Send only that diagnostic and the
non-secret final line.

Verify the persisted account:

~~~
railway ssh --service launcher-restore-worker -- sh -lc "export HOME=/var/lib/launcher/megacmd; exec gosu launcher /usr/local/bin/launcher-admin storage accounts list --provider-id mega-cold"
~~~

The account configuration file is on the Railway volume at
`/var/lib/launcher/megacmd/mega-accounts.json`. It contains references and
paths, not a plaintext password.

## 13. Run the MEGA smoke and enable the cold gate

From Windows:

~~~
.\scripts\staging\mega-smoke.ps1 -Railway -WorkerService launcher-restore-worker
~~~

Success:

~~~
check=COLD_put status=PASS
check=COLD_head status=PASS
check=COLD_get status=PASS
check=COLD_delete status=PASS
storage_smoke=PASS
mega_smoke=PASS
~~~

Now run:

~~~
.\scripts\staging\verify-staging.ps1 -RequireCold
~~~

Success: `staging_verify=PASS` with a healthy HOT provider and one healthy
COLD account.

## 14. Generate the launcher staging configuration

After the API HTTPS domain and public key exist:

~~~
launcher-admin configure-staging --api-url https://<generated-api-domain> --public-key .\artifacts\staging-keys\staging-2026-01.public.pem --output .\artifacts\launcher-staging.json
~~~

If `launcher-admin` is not on `PATH`, use:

~~~
cargo run --manifest-path server/Cargo.toml -p launcher-worker --bin launcher-admin -- configure-staging --api-url https://<generated-api-domain> --public-key .\artifacts\staging-keys\staging-2026-01.public.pem --output .\artifacts\launcher-staging.json
~~~

Success: the generated file contains the API URL and only the
`staging-2026-01` public key. Do not put this file into the production
launcher configuration.

## 15. Publish synthetic A and B

Use only the fixture from `artifacts\staging-fixture`. Run the prepared
publisher from the repository root:

~~~
.\scripts\staging\publish-synthetic.ps1 -WorkerService launcher-restore-worker
~~~

The script packages and signs A and B locally, uploads only those synthetic
packages to `/var/lib/launcher/staging-publish`, and runs the existing
`launcher-admin publish` command inside the private worker. Railway injects
the database, Bucket, and MEGA variables into that worker; no secret is
copied to the Windows process. It removes the temporary remote package
directory after successful publication. Use `-KeepRemotePackages` only for
debugging a failed publish.

If `LAUNCHER_SIGNING_PRIVATE_KEY_PEM` is set as a sealed worker secret, the
script can use it for the signing step when the local private-key file is
not present. It never prints the key.

Success: each command reports verified publication and the policy remains
satisfied. If publication fails, send the stage and error code only.

## 16. Run the measured remote A -> B test

After both builds are published:

~~~
.\scripts\staging\run-remote-ab.ps1 -ApiUrl $env:LAUNCHER_STAGING_API_URL -SettingsPath .\artifacts\launcher-staging.json -SourceA .\artifacts\staging-fixture\A -SourceB .\artifacts\staging-fixture\B -BuildAId staging-a -BuildBId staging-b
~~~

The script performs a real remote install and update, verifies byte identity
against both source directories, checks that the resolved chunk host is not
the API host, and prints measured:

~~~
build_a_encoded_bytes
build_b_encoded_bytes
network_downloaded_bytes
local_cache_reuse_bytes
savings_percent
~~~

It does not print URLs or credentials. Do not substitute the local 85.34%
baseline for the output of this command.

## 17. Prove Range resume and presigned URL refresh

Range/resume:

~~~
.\scripts\staging\range-resume-test.ps1 -ApiUrl $env:LAUNCHER_STAGING_API_URL -BuildId staging-b
~~~

Success requires two real HTTP `206` responses, a partial size, a final size,
and a BLAKE3 equal to the manifest hash. An HTTP 200 response is reported as
failure, not counted as resume evidence.

Presigned expiry requires a short staging TTL. Temporarily set
`LAUNCHER_S3_PRESIGN_TTL_SECONDS=2` on the API service, deploy, then run:

~~~
.\scripts\staging\presigned-expiry-test.ps1 -ApiUrl $env:LAUNCHER_STAGING_API_URL -BuildId staging-b -WaitSeconds 4
~~~

Restore the normal TTL of `900` and redeploy after the test. Success requires
an expired first URL, a failed request, a new resolve, and a successful
download with the expected BLAKE3.

## 18. Run the guarded cold -> HOT restore test

Choose a chunk hash from `staging-b` that is not shared by another build.
The command itself checks the build prefix and published-build reference
count. Stop the long-running worker briefly so it cannot race the deliberate
HOT deletion.

~~~
.\scripts\staging\cold-restore-test.ps1 -Railway -WorkerService launcher-restore-worker -BuildId staging-b -EncodedHash <64-lowercase-hex-hash> -Confirm
~~~

The command deletes only the selected HOT object, removes its metadata,
restores from COLD, verifies BLAKE3, and records the restored HOT location.
Success:

~~~
cold_restore=PASS
cold_restore_test=PASS
~~~

If the hash is shared, the command refuses to run. Do not bypass that check
and do not select a production build.

## 19. Restart and recovery evidence

Run these only after the smoke and restore tests pass:

1. Redeploy or restart the API. Confirm `/v1/health` returns 200 and
   `/v1/ready` returns `status: "ready"`.
2. Restart the worker. Confirm the same persistent MEGAcmd session is reused
   and `storage accounts list` still shows the account.
3. During a queued restore, restart the worker and confirm the lease is
   recovered without duplicate corruption.
4. Review Railway logs for secrets, passwords, private keys, full presigned
   URLs, or chunk bodies. Report `PASS` or the redacted line that failed.

Do not declare staging validated until the remote A/B, Range, expiry refresh,
MEGA smoke, cold restore, restart recovery, and security-log checks all have
actual evidence.

## Stop/report rules

Send only:

- service status and deployment IDs;
- HTTP status codes and non-secret check names;
- measured byte counts and timings;
- diagnostics such as `MEGA_NETWORK_UNAVAILABLE` or `MEGA_AUTH_FAILED`;
- whether a secret is set, never its value.

Never send Railway tokens, database URLs, S3 keys, MEGA passwords, session
files, private signing keys, or complete presigned URLs.
