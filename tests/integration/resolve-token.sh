#!/usr/bin/env bash
# Print the API token that the disposable NetBox fixture expects.
#
# NetBox 4.5 stores the seeded token secret separately from a generated public
# key and expects callers to send `nbt_<key>.<secret>`. Older fixtures accept the
# bare deterministic secret, so this helper is only needed by the 4.5 GraphQL CI
# lane after the app has finished first-boot initialization.
set -euo pipefail

COMPOSE_FILE="${COMPOSE_FILE:-tests/integration/docker-compose.yml}"
NETBOX_TOKEN="${NETBOX_TOKEN:-0123456789abcdef0123456789abcdef0fedcba9}"
NETBOX_USERNAME="${NETBOX_USERNAME:-admin}"

# Read the token's public key. `filter().first()` (not `get()`) so a missing
# token prints an empty line instead of raising `DoesNotExist` (a traceback that
# `tail` would otherwise turn into a bogus "key"). stderr is dropped so only the
# key reaches stdout.
key="$(
  docker compose -f "${COMPOSE_FILE}" exec -T netbox \
    /opt/netbox/venv/bin/python /opt/netbox/netbox/manage.py shell -c \
    "from users.models import Token; t = Token.objects.filter(user__username='${NETBOX_USERNAME}').first(); print(t.key if t else '')" \
    2>/dev/null | tail -n 1
)"

# Fail LOUD if the image never provisioned the token. Otherwise an empty key
# silently yields a bogus token and surfaces downstream as a misleading "NetBox
# did not become ready (HTTP 403)" timeout (see the 4.6 lane regression: the
# netbox-docker entrypoint now needs SUPERUSER_API_KEY *and* SUPERUSER_API_TOKEN).
if [ -z "${key}" ]; then
  echo "resolve-token: no API token found for user '${NETBOX_USERNAME}' on this fixture." >&2
  echo "  The NetBox image did not provision the superuser token — check that both" >&2
  echo "  SUPERUSER_API_TOKEN and SUPERUSER_API_KEY are set in docker-compose.yml." >&2
  exit 1
fi

case "${key}" in
  nbt_*)
    printf '%s.%s\n' "${key}" "${NETBOX_TOKEN}"
    ;;
  "${NETBOX_TOKEN}")
    printf '%s\n' "${NETBOX_TOKEN}"
    ;;
  *)
    printf 'nbt_%s.%s\n' "${key}" "${NETBOX_TOKEN}"
    ;;
esac
