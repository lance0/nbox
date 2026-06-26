# NetBox Compatibility

nbox targets **NetBox 4.2+** over the REST API and is verified through **4.6.2** (the
current stable). Each release in the 4.2–4.6 range moved an API contract nbox
depends on; nbox handles the differences at runtime (version probe + schema probe)
rather than pinning a single version. The table below is the matrix; the behavior is
pinned by `tests/compat_tests.rs`.

> NetBox uses a tick-tock cadence: even minors (4.4, 4.6) are non-breaking, odd
> minors (4.5, 4.7) may break. **4.7 ("tock") is the next compatibility watch.**

The floor is enforced on connect via `/api/status/`
(`netbox_version` < 4.2 → fail fast). `nbox status --json` and MCP `nbox_status`
report the connected version, the build's `minimum_supported`, a `compatible`
flag, and the per-surface backend routing.

## Matrix

| Concern | 4.2 | 4.3 | 4.5 / 4.6 |
|---|---|---|---|
| Scope model | polymorphic `scope` (`scope_type` + `scope_id`); prefix `site` FK dropped ¹ | same | same |
| Search backend | REST `q=` fan-out | REST (GraphQL has no `q`) | REST (GraphQL has no `q`) |
| GraphQL filtering | per-field input objects | advanced per-field lookups (AND/OR, custom fields) ² | same |
| Prefix `utilization` | returned by the REST API | returned by the REST API | not returned → computed client-side ³ |
| `/api/status/` auth | open by default | open by default | requires auth ³ |
| Token scheme | v1 `Authorization: Token` | v1 `Token` | v1 `Token` **+ v2 `Authorization: Bearer nbt_…`** ¹ |
| Write concurrency | no `ETag`/`If-Match`; read-before-write fallback ⁵ | same | **`ETag` + `If-Match` (412)**; 4.2–4.5 keep the fallback ⁵ |

¹ In the official NetBox release notes — the `4.2.0` scope change (Jan 2025) and the `4.5` v2 tokens (HMAC, `nbt_` prefix, `Bearer`). v1 tokens are **deprecated but retained through the 4.x line; removal is planned for v5.0** (4.6 pushed this out from the originally-announced 4.7). nbox auto-detects the scheme, so the timeline doesn't affect it.

⁴ NetBox 4.6 adds `GRAPHQL_MAX_QUERY_DEPTH`. nbox's GraphQL accelerators (the VRF and route-target bundles) issue nested queries; unsupported schemas resolve to REST in `nbox status`, and a runtime bundle failure (including a low depth cap) retries the same detail over REST with a warning. Search is REST regardless, so it's unaffected.
² NetBox [#7598](https://github.com/netbox-community/netbox/issues/7598), "adopt advanced query filtering in GraphQL." GraphQL never had a REST-style full-text `q`; this rework is why a per-kind GraphQL search can't stand in for REST search.
³ **Observed** against live instances (4.2 vs 4.5.10), **not called out in the release notes** — so treat as empirical, not a documented contract. `/api/status/` auth may reflect instance `LOGIN_REQUIRED`-style config rather than a strict version change; either way nbox authenticates **every** request (including the version probe), so it is unaffected.
⁵ NetBox 4.6 returns an `ETag` on REST object-detail responses and honors `If-Match` on writes (a stale object yields `412 Precondition Failed`). The safe-write engine (ADR-0001 §3) records the `ETag` on the read-before-write and sends `If-Match` on apply when present; on 4.2–4.5 (no `ETag`) it falls back to a `last_updated` + before-hash read-before-write check. So 4.6+ gets race protection in one `PATCH`; older releases get a conservative re-read guard. Writes are off by default behind `--allow-writes` + confirmation.

## How nbox adapts

- **Scope (4.2).** Prefixes/VLANs/clusters use the polymorphic `scope`, so a plain
  `?site=` slug filter is dead on those endpoints. `--site`/`--region`/
  `--site-group`/`--location` resolve to a numeric id once, then go out-of-band
  per endpoint: the scoped endpoints (prefixes, clusters) keep exact `--site`
  with `scope_type=dcim.site` + `scope_id=<id>` and use NetBox's tree-aware
  `region_id`/`site_group_id`/`location_id` filters for hierarchical scopes; the
  rest get `site_id`/`region_id`/… An endpoint with no clean filter for the active
  scope skips itself rather than return an unfiltered set.

- **Search is always REST.** NetBox 4.3 reworked GraphQL filtering into advanced
  per-field lookups (AND/OR, custom fields — NetBox #7598). GraphQL has no
  REST-style full-text `q`, so a per-kind GraphQL search can't reproduce canonical
  search; `nbox search` is a parallel REST `q=` fan-out across the object
  endpoints. Even with a
  `graphql` preference for the search surface, the backend resolves to a REST
  fallback (without probing the schema) and `status` carries the reason. GraphQL
  stays an opt-in accelerator for the VRF and route-target views.

- **Client-side container utilization.** As of NetBox 4.5 the prefix REST API no
  longer returns `utilization` (observed live — present on 4.2, absent on 4.5's
  list, detail, and OpenAPI schema; not called out in the release notes). nbox
  computes a container prefix's
  utilization from the already-fetched tree — the fraction of its space its direct
  children cover, `Σ 2^(parent_len − child_len)` — so no extra calls, on every
  version. An older NetBox that still serves `utilization` keeps its richer value;
  a leaf (no children) has no client-side value (IP-level utilization would need
  per-prefix queries).

- **Token scheme (4.5).** v2 tokens are shaped `nbt_<key>.<secret>` and sent as
  `Authorization: Bearer`; legacy v1 tokens as `Authorization: Token`. The default
  `auth_scheme = "auto"` detects the shape; force one with `auth_scheme =
  "bearer"`/`"token"` on the profile.

- **NAT inside/outside (4.6).** NetBox 4.6 embeds `nat_inside` (a brief IP ref, on
  the *outside* IP) and `nat_outside` (an array, on the *inside* IP) on the
  full `IPAddress` object. `nbox ip` surfaces both when present and omits them
  when absent, so a non-NAT IP's output is byte-identical on every version (the
  fields simply deserialize to `None`/empty on pre-4.6). No version gate — the
  enrichment is additive and free-when-absent.

- **`virtual-circuit` (4.2).** The `circuits/virtual-circuits/` and
  `circuits/virtual-circuit-terminations/` endpoints ship in 4.2+ (nbox's whole
  supported range), so there's no version gate. The `owner` scalar arrived in
  4.5; it's an `Option` with a `#[serde(default)]`, so a pre-4.5 virtual circuit
  deserializes fine and the field is simply omitted from the view (additive and
  free-when-absent, like the NAT fields).

- **`owner` field + filters (4.5).** NetBox 4.5 added a native `owner` (a user
  **or** group) on most objects. nbox surfaces it on every detail view as a
  friendly label, omitted when absent (an `Option` with `#[serde(default)]` on
  each model — byte-identical for pre-4.5 objects). In `search`, `--owner` /
  `--owner-group` map to `owner=` / `owner_group=` params on every search
  endpoint (no resolution step — the server matches by name); owner is
  polymorphic, so the two are separate filters, and both are silently ignored
  on releases that carry no owner data. No version gate — additive and
  free-when-absent.

- **`rack-group` + `vm-type` kinds (4.6).** NetBox 4.6 adds `dcim/rack-groups/`
  and `virtualization/virtual-machine-types/` as distinct, listable object kinds.
  Both are simple name/slug/description objects with a relation count
  (`rack_count` / `virtual_machine_count`) plus `owner` (4.5) and the usual
  `tags`/`custom_fields`. They're full first-class kinds on nbox (`nbox <kind>`,
  `nbox_get`, `nbox journal`, `nbox open`, `nbox://` resource, `nbox search`
  fan-out). Model shapes verified against the live 4.6.2 OpenAPI schema. The
  third 4.6 kind, `cable-bundle`, is deferred — it pairs with the cable-path
  visualizer. No version gate: on a pre-4.6 instance these endpoints 404 and nbox
  reports not-found (exit `4`), like any absent kind.

- **GraphQL schema probe.** When a surface opts into GraphQL, nbox probes the live
  schema (filter input shapes + pagination) instead of hard-coding a version, so
  4.2/4.3/4.5+ differences are absorbed, and falls back to REST (with the reason in
  `status`) when the schema can't back the surface.

- **Credential preflight (4.5).** `/api/status/` is reachable **without** a valid
  token on an instance configured with `LOGIN_REQUIRED=False`, so a 200 status
  response can hide a bad/expired token — nbox can't infer token validity from the
  status fetch. NetBox 4.5 added a dedicated `/api/authentication-check/` endpoint
  (gated on `IsAuthenticated`; returns the flat `UserSerializer` body), and `nbox
  status` / MCP `nbox_status` now run it as a best-effort preflight and surface the
  verdict in a `token` field: `valid` (carrying the authenticated username/display),
  `invalid` (HTTP 401/403 — the token was rejected, with the server's reason), or
  `unverified` (the endpoint is absent on NetBox < 4.5, or the probe could not run).
  It never errors, so it can't turn a successful status fetch into a failure; on an
  auth-required instance the status fetch itself rejects a bad token (exit 3)
  before the preflight runs. The exit-code contract for `nbox status` is unchanged
  — a rejected token during the status fetch still exits 3; the preflight is
  informational (the `token` field), not an exit trigger.
