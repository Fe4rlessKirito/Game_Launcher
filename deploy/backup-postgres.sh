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
  exec -T postgres sh -ceu 'pg_dump --username="$POSTGRES_USER" --format=custom --no-owner --no-acl --file=- "$POSTGRES_DB"' > "${output}"
chmod 600 -- "${output}"
sha256sum -- "${output}" > "${output}.sha256"
chmod 600 -- "${output}.sha256"

# This is intentionally scoped to the explicit backup directory and dump
# suffix. Copy the new dump and checksum off-host before the retention window.
find "${backup_dir}" -maxdepth 1 -type f -name 'postgres-*.dump' -mtime "+${retention_days}" -delete
find "${backup_dir}" -maxdepth 1 -type f -name 'postgres-*.dump.sha256' -mtime "+${retention_days}" -delete
printf 'backup=%s bytes=%s sha256=%s\n' \
  "${output}" "$(stat -c '%s' "${output}")" "$(cut -d' ' -f1 "${output}.sha256")"
