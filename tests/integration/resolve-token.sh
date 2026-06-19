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

key="$(
  docker compose -f "${COMPOSE_FILE}" exec -T netbox \
    /opt/netbox/venv/bin/python /opt/netbox/netbox/manage.py shell -c \
    "from users.models import Token; print(Token.objects.get(user__username='${NETBOX_USERNAME}').key)" \
    | tail -n 1
)"

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
