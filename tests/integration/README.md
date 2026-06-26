# NetBox integration fixture

End-to-end checks that run the compiled `nbox` binary against real, pinned NetBox
fixtures. This catches what the offline wiremock suite can't: polymorphic scope
filters, pagination offset windows, available-prefix/IP shapes, GraphQL schema
drift, and the serializer/detail-model shapes of the live API.

These are heavy, so they live behind a separate workflow
(`.github/workflows/netbox-integration.yml`) and the Rust tests are all
`#[ignore]`d — plain `cargo test` skips them.

## Pieces

- `docker-compose.yml` — boots one pinned NetBox image (default
  `netboxcommunity/netbox:v4.2.9`, override with `NETBOX_IMAGE`) plus Postgres
  and Redis. The API is exposed on host port **8000** (`http://localhost:8000`).
  Postgres runs on a `tmpfs`, so every `up` starts from a clean DB.
- `wait-for-ready.sh` — polls `/api/status/` until NetBox answers 200 (or times
  out). First boot runs migrations + superuser creation, so this can take a
  couple of minutes on a cold image.
- `resolve-token.sh` — prints the NetBox 4.5+ v2 API token assembled from the
  generated public key and the deterministic fixture secret.
- `seed.py` — creates the fixture (stdlib only, no pip installs). Idempotent.
- `../it_netbox.rs` — the gated Rust tests.

## Token

The token is **deterministic** and shared by the compose file, the seed, the
tests, and CI:

```
0123456789abcdef0123456789abcdef0fedcba9
```

The NetBox image's entrypoint creates the superuser and a `Token` with this exact
key on first boot (via `SUPERUSER_API_TOKEN` in `docker-compose.yml`). Because
the DB is ephemeral (`tmpfs`), this is always the token in play. The tests and
seed read it from `NETBOX_TOKEN`, falling back to this default.

NetBox 4.5+ stores the deterministic secret behind a generated v2 token key.
For those images, wait until first boot finishes, then export the resolved token:

```sh
export NETBOX_TOKEN="$(./tests/integration/resolve-token.sh)"
```

## Fixture created by `seed.py`

- tag `ci-tag`
- site `ci-site`
- VRF `ci-vrf`
- prefix `10.10.0.0/16` scoped to the site (`scope_type=dcim.site`)
- prefixes `10.20.0.0/16`, `10.30.0.0/16`, `10.40.0.0/16`, and
  `10.41.0.0/16` scoped to region/site-group/location/child-location fixtures
  for hierarchical search-scope checks
- duplicate prefix `10.0.0.0/24` in **two** tables: VRF `ci-vrf` and global
  (exercises `--vrf` disambiguation / ambiguity)
- VLAN vid `1234` (`ci-vlan`) at `ci-site`
- manufacturer `ci-mfg` → device-type `ci-model` → device-role `ci-role` →
  device `ci-dev1` at `ci-site`
- interface `xe-0/0/1` on `ci-dev1` (name has a slash)
- IP `10.10.0.5/24` on that interface, set as the device's primary IPv4
- 25 child prefixes `10.10.1.0/24 .. 10.10.25.0/24` nested under the /16, to
  force multi-page pagination on the prefix detail's child-prefix list

## Run it locally

```sh
docker compose -f tests/integration/docker-compose.yml up -d
./tests/integration/wait-for-ready.sh
./tests/integration/seed.py

NBOX_URL=http://localhost:8000 \
  NETBOX_TOKEN=0123456789abcdef0123456789abcdef0fedcba9 \
  cargo test --test it_netbox -- --ignored --skip graphql_backend

docker compose -f tests/integration/docker-compose.yml down -v
```

`NBOX_URL` and `NETBOX_TOKEN` default to the values above, so exporting them is
optional when using this compose file unchanged.

For the GraphQL compatibility lane against NetBox 4.5:

```sh
NETBOX_IMAGE=netboxcommunity/netbox:v4.5.10-4.0.2 \
  docker compose -f tests/integration/docker-compose.yml up -d
NETBOX_READY_HTTP_CODES=200,403 ./tests/integration/wait-for-ready.sh
export NETBOX_TOKEN="$(./tests/integration/resolve-token.sh)"
./tests/integration/wait-for-ready.sh
./tests/integration/seed.py
cargo test --test it_netbox graphql_backend -- --ignored --test-threads=1
docker compose -f tests/integration/docker-compose.yml down -v
```

## Bumping the NetBox version

Set `NETBOX_IMAGE` to a specific pinned tag, re-run the local flow above to
confirm it's still green, then commit the new workflow/default only if it is
intentional.
