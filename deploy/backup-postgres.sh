#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root on the Mantle VPS. The destination must be on
# storage that is backed up or copied off-host; the Docker volume alone is not
# a backup strategy.
backup_dir="${BACKUP_DIR:-/var/backups/vaultnode}"
retention_days="${BACKUP_RETENTION_DAYS:-14}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p -- "${backup_dir}"
chmod 700 -- "${backup_dir}"

output="${backup_dir}/postgres-${timestamp}.dump"
docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml \
  exec -T postgres sh -ceu 'pg_dump --username="$POSTGRES_USER" --format=custom --no-owner --no-acl "$POSTGRES_DB"' > "${output}"
chmod 600 -- "${output}"
sha256sum -- "${output}" > "${output}.sha256"
chmod 600 -- "${output}.sha256"

# Optional off-host replication is explicit and fail-closed when required.
# Strict host-key checking is intentional: configure the host key in the
# invoking user's known_hosts before enabling this path.
replication_host="${BACKUP_REPLICATION_HOST:-}"
replication_user="${BACKUP_REPLICATION_USER:-backup}"
replication_dir="${BACKUP_REPLICATION_DIR:-}"
replication_key="${BACKUP_REPLICATION_IDENTITY_FILE:-}"
replication_required="${BACKUP_REPLICATION_REQUIRED:-false}"

if [[ -n "${replication_host}" || -n "${replication_dir}" || -n "${replication_key}" ]]; then
  [[ -n "${replication_host}" && -n "${replication_dir}" && -n "${replication_key}" ]] || {
    echo 'backup replication requires host, directory, and identity file' >&2
    exit 1
  }
  [[ "${replication_dir}" =~ ^/[A-Za-z0-9._/-]+$ ]] || {
    echo 'backup replication directory must be an absolute safe path' >&2
    exit 1
  }
  [[ -r "${replication_key}" ]] || {
    echo 'backup replication identity file is not readable' >&2
    exit 1
  }
  ssh_options=(-o BatchMode=yes -o StrictHostKeyChecking=yes -i "${replication_key}")
  replication_remote="${replication_user}@${replication_host}"
  ssh "${ssh_options[@]}" "${replication_remote}" "install -d -m 700 -- '${replication_dir}'"
  scp "${ssh_options[@]}" "${output}" "${output}.sha256" \
    "${replication_remote}:${replication_dir}/"
  printf 'backup_replication=PASS host=%s directory=%s\n' \
    "${replication_host}" "${replication_dir}"
elif [[ "${replication_required}" == 'true' ]]; then
  echo 'backup replication is required but not configured' >&2
  exit 1
fi

# This is intentionally scoped to the explicit backup directory and dump
# suffix. Replication happens before retention cleanup.
find "${backup_dir}" -maxdepth 1 -type f -name 'postgres-*.dump' -mtime "+${retention_days}" -delete
find "${backup_dir}" -maxdepth 1 -type f -name 'postgres-*.dump.sha256' -mtime "+${retention_days}" -delete
printf 'backup=%s bytes=%s sha256=%s\n' \
  "${output}" "$(stat -c '%s' "${output}")" "$(cut -d' ' -f1 "${output}.sha256")"
