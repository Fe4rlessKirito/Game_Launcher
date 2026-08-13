#!/usr/bin/env bash
set -euo pipefail

base_url="${LAUNCHER_PUBLIC_BASE_URL:?set LAUNCHER_PUBLIC_BASE_URL}"
operator_token="${LAUNCHER_OPERATOR_TOKEN:?set LAUNCHER_OPERATOR_TOKEN}"

if [[ "${LAUNCHER_ALLOW_HTTP_HEALTHCHECK:-false}" != "true" && "$base_url" != https://* ]]; then
  printf 'mantle health check requires an HTTPS public URL; set LAUNCHER_ALLOW_HTTP_HEALTHCHECK=true only for local/IP smoke tests\n' >&2
  exit 1
fi

curl --fail --silent --show-error --max-time 15 "${base_url%/}/v1/health" >/dev/null
curl --fail --silent --show-error --max-time 15 "${base_url%/}/v1/ready" >/dev/null
curl --fail --silent --show-error --max-time 15 \
  -H "Authorization: Bearer ${operator_token}" \
  "${base_url%/}/metrics" >/dev/null
printf 'mantle_api=PASS readiness=PASS operator_metrics=PASS\n'
