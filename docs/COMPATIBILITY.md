# NetBox Compatibility

nbox targets **NetBox 4.2+** over the REST API. Each release in the 4.2–4.5 range
moved an API contract nbox depends on; nbox handles the differences at runtime
(version probe + schema probe) rather than pinning a single version. The table
below is the matrix; the behavior is pinned by `tests/compat_tests.rs`.

The floor is enforced on connect via `/api/status/`
(`netbox_version` < 4.2 → fail fast). `nbox status --json` and MCP `nbox_status`
report the connected version, the build's `minimum_supported`, a `compatible`
flag, and the per-surface backend routing.

## Matrix

| Concern | 4.2 | 4.3 | 4.5+ |
|---|---|---|---|
| Scope model | polymorphic `scope` (`scope_type` + `scope_id`); prefix `site` FK dropped | same | same |
| Search backend | REST (full-text `q`) | **REST only** — GraphQL `q` dropped | REST only |
| GraphQL filter shape | per-field input objects | per-field Strawberry lookups | per-field Strawberry lookups |
| Prefix `utilization` source | REST API field | REST API field | **client-side** — API field dropped |
| `/api/status/` auth | unauthenticated | unauthenticated | **requires auth** |
| Token scheme | v1 `Authorization: Token` | v1 `Token` | v1 `Token` **+ v2 `Authorization: Bearer nbt_…`** |

## How nbox adapts

- **Scope (4.2).** Prefixes/VLANs/clusters use the polymorphic `scope`, so a plain
  `?site=` slug filter is dead on those endpoints. `--site`/`--region`/`--group`/
  `--location` resolve to a numeric id once, then go out-of-band per endpoint: the
  polymorphic endpoints (prefixes, clusters) get `scope_type=dcim.<kind>` +
  `scope_id=<id>`; the rest get `site_id`/`region_id`/… An endpoint with no clean
  filter for the active scope skips itself rather than return an unfiltered set.

- **Search is always REST (4.3).** NetBox 4.3 moved GraphQL filtering to per-field
  lookups and dropped the full-text `q`, which has no GraphQL equivalent. `nbox
  search` is a parallel REST `q=` fan-out across the object endpoints. Even with a
  `graphql` preference for the search surface, the backend resolves to a REST
  fallback (without probing the schema) and `status` carries the reason. GraphQL
  stays an opt-in accelerator for the VRF view only.

- **Client-side container utilization (4.5).** NetBox 4.5 dropped the prefix
  `utilization` field from the REST API. nbox computes a container prefix's
  utilization from the already-fetched tree — the fraction of its space its direct
  children cover, `Σ 2^(parent_len − child_len)` — so no extra calls, on every
  version. An older NetBox that still serves `utilization` keeps its richer value;
  a leaf (no children) has no client-side value (IP-level utilization would need
  per-prefix queries).

- **Token scheme (4.5).** v2 tokens are shaped `nbt_<key>.<secret>` and sent as
  `Authorization: Bearer`; legacy v1 tokens as `Authorization: Token`. The default
  `auth_scheme = "auto"` detects the shape; force one with `auth_scheme =
  "bearer"`/`"token"` on the profile.

- **GraphQL schema probe.** When a surface opts into GraphQL, nbox probes the live
  schema (filter input shapes + pagination) instead of hard-coding a version, so
  4.2/4.3/4.5+ differences are absorbed, and falls back to REST (with the reason in
  `status`) when the schema can't back the surface.
