#!/usr/bin/env bash
# Poll NetBox's /api/status/ until it answers with an accepted HTTP code, or
# time out.
#
# NetBox's first boot runs DB migrations + superuser creation before the API
# serves, so this can take a couple of minutes on a cold image. We poll rather
# than sleep a fixed amount so the seed/test steps start the instant it's ready.
#
# NetBox 4.2 requires authentication on /api/status/ (it returns 403, not 200,
# without a token), so the probe sends the deterministic CI token.
#
#   NBOX_URL=http://localhost:8000 NETBOX_TOKEN=<token> ./wait-for-ready.sh [timeout_seconds]
#   NETBOX_READY_HTTP_CODES=200,403 ./wait-for-ready.sh [timeout_seconds]
#
# Defaults: NBOX_URL=http://localhost:8000, the seeded token, timeout 300s.
set -euo pipefail

NBOX_URL="${NBOX_URL:-http://localhost:8000}"
NETBOX_TOKEN="${NETBOX_TOKEN:-0123456789abcdef0123456789abcdef0fedcba9}"
NETBOX_READY_HTTP_CODES="${NETBOX_READY_HTTP_CODES:-200}"
TIMEOUT="${1:-300}"
INTERVAL=3

status_url="${NBOX_URL%/}/api/status/"
deadline=$(( $(date +%s) + TIMEOUT ))

is_ready_code() {
  case ",${NETBOX_READY_HTTP_CODES}," in
    *,"$1",*) return 0 ;;
    *) return 1 ;;
  esac
}

echo "Waiting for NetBox at ${status_url} (HTTP ${NETBOX_READY_HTTP_CODES}; timeout ${TIMEOUT}s)..."
while :; do
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
    -H "Authorization: Token ${NETBOX_TOKEN}" "${status_url}" || echo 000)"
  if is_ready_code "${code}"; then
    echo "NetBox is ready (HTTP ${code} from /api/status/)."
    exit 0
  fi
  if [ "$(date +%s)" -ge "${deadline}" ]; then
    echo "ERROR: NetBox did not become ready within ${TIMEOUT}s (last HTTP code: ${code})." >&2
    exit 1
  fi
  sleep "${INTERVAL}"
done
