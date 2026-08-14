#!/usr/bin/env bash
set -euo pipefail

# Fail-closed preflight for the public Mantle cutover. This intentionally does
# not accept -k, an IP URL, or a missing off-host backup configuration: a local
# HTTP smoke is already covered by mantle-healthcheck.sh.

base_url="${LAUNCHER_PUBLIC_BASE_URL:?set LAUNCHER_PUBLIC_BASE_URL to the final HTTPS URL}"
site_host="${SITE_HOST:?set SITE_HOST to the final public hostname}"
acme_email="${ACME_EMAIL:?set ACME_EMAIL for the Caddy ACME account}"
operator_token="${LAUNCHER_OPERATOR_TOKEN:?set LAUNCHER_OPERATOR_TOKEN}"
expected_ip="${MANTLE_PUBLIC_IP:-}"

[[ "${base_url}" == https://* ]] || {
  echo 'production check requires LAUNCHER_PUBLIC_BASE_URL=https://...' >&2
  exit 1
}
[[ "${site_host}" != *://* && "${site_host}" != */* ]] || {
  echo 'SITE_HOST must be a hostname, not a URL or path' >&2
  exit 1
}
[[ "${acme_email}" == *@*.* ]] || {
  echo 'ACME_EMAIL does not look like an email address' >&2
  exit 1
}

url_host="$(printf '%s\n' "${base_url}" | sed -E 's#^https://([^/:]+).*$#\1#')"
[[ "${url_host}" == "${site_host}" ]] || {
  echo 'LAUNCHER_PUBLIC_BASE_URL host must match SITE_HOST' >&2
  exit 1
}

resolved="$(getent ahostsv4 "${site_host}" 2>/dev/null | awk '{print $1}' | sort -u || true)"
if [[ -z "${resolved}" ]]; then
  echo "DNS A record is missing for ${site_host}" >&2
  exit 1
fi
if [[ -n "${expected_ip}" ]] && ! grep -Fxq "${expected_ip}" <<<"${resolved}"; then
  echo "DNS for ${site_host} does not point at MANTLE_PUBLIC_IP=${expected_ip}" >&2
  printf 'resolved=%s\n' "${resolved}" >&2
  exit 1
fi

request() {
  curl --fail --silent --show-error --location --max-time 20 "$@"
}

health="$(request "${base_url%/}/v1/health")"
ready="$(request "${base_url%/}/v1/ready")"
metrics="$(request -H "Authorization: Bearer ${operator_token}" "${base_url%/}/metrics")"

grep -Fq '"status":"ok"' <<<"${health}" || {
  echo 'public HTTPS health response did not report status=ok' >&2
  exit 1
}
grep -Fq '"status":"ready"' <<<"${ready}" || {
  echo 'public HTTPS readiness response did not report status=ready' >&2
  exit 1
}
grep -Fq '"operator_auth_configured":true' <<<"${ready}" || {
  echo 'public HTTPS readiness response did not confirm operator authentication' >&2
  exit 1
}
grep -Fq 'launcher_storage_' <<<"${metrics}" || {
  echo 'authenticated public metrics response is missing launcher storage gauges' >&2
  exit 1
}

# Caddy should redirect the plaintext listener once the HTTPS file is active.
http_status="$(curl --silent --show-error --max-time 20 \
  -o /dev/null -w '%{http_code}' "http://${site_host}/v1/health")"
case "${http_status}" in
  301|302|307|308) ;;
  *)
    echo "plaintext listener returned HTTP ${http_status}; expected an HTTPS redirect" >&2
    exit 1
    ;;
esac

# A production-shaped deployment must have an off-host destination configured.
[[ "${BACKUP_REPLICATION_REQUIRED:-false}" == true ]] || {
  echo 'BACKUP_REPLICATION_REQUIRED must be true for the production cutover' >&2
  exit 1
}
[[ -n "${BACKUP_REPLICATION_HOST:-}" && \
   -n "${BACKUP_REPLICATION_DIR:-}" && \
   -n "${BACKUP_REPLICATION_IDENTITY_FILE:-}" ]] || {
  echo 'off-host backup replication host, directory, and identity file are required' >&2
  exit 1
}

printf 'mantle_production=PASS dns=PASS https=PASS readiness=PASS metrics=PASS backup_replication=configured\n'
