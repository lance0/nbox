# Architecture

nbox keeps a hard line between the **wire layer** (what NetBox returns) and the
**view models** (what nbox renders). The same view models feed the CLI and the
TUI, so output is consistent across both.

## Layers

- **`netbox/`** — NetBox clients and wire types. REST is canonical and powers
  search, identity resolution, detail lookups, journals, `raw`, and the
  available-IP/prefix math; GraphQL is an opt-in accelerator for the VRF and
  route-target views.
  - `client.rs` — auth, paging, timeouts; retries HTTP 429 (`Retry-After` +
    backoff); maps statuses to typed errors (401→auth, 403→perms, 404→not-found).
    The same authenticated client owns `/graphql/` POSTs. Holds the profile's
    per-surface `ApiConfig` and exposes `api_preference`/`effective_backend`.
  - `endpoints.rs` — endpoint paths.
  - `pagination.rs` — `Page<T>`, offset paging (`list` / `list_all`).
  - `query.rs` — per-object resolvers (`*_by_ref`, candidates, scope labels).
  - `capabilities.rs` — resolves a surface's configured preference + live schema
    probe into an `EffectiveBackend` (with REST-fallback reason); the surface-aware
    capability report and `status.api` routing.
  - `graphql.rs` — all GraphQL: schema capability probing (filter input shapes and
    pagination across NetBox 4.2/4.3/4.5+) and `graphql_vrf_bundle` (the single-POST
    VRF prefixes+addresses query that backs the VRF view when opted in).
  - `search.rs` — parallel REST `q=` fan-out → ranked, deduped `SearchOutcome`.
    Always REST: NetBox's GraphQL has no `q` full-text equivalent (4.3+ moved to
    per-field filters), so it can't reproduce canonical search.
  - `models/` — permissive wire structs (`dcim`, `ipam`, `circuits`, `extras`,
    `tenancy`, `common`). Nullable, brief/complete, unknown fields ignored.
- **`domain/`** — flattened view models, one per object (`device_detail`,
  `interface_view`, `ip_view`, `prefix_view`, `circuit_view`, …). These never
  leak raw API shapes; they own plain-text rendering and `Serialize` for JSON.
- **`output/`** — `Format` (plain/json/csv) and the shared `emit()` path
  (`--fields`/`--raw`/`--envelope` for JSON; generic CSV).
- **`tui/`** — a ratatui app. Input handling (`state.rs`) is **pure**:
  `handle_event` mutates state and returns `Vec<AppCommand>`; the loop
  (`app.rs`) **spawns** network commands and posts results back as events —
  nothing blocks the render loop.
- **`error.rs`** — `NboxError` with stable exit codes (see below).
- **`config.rs`** — typed config, profiles, token resolution, format-preserving
  writes (`toml_edit`).
- **`cache/`** — a small, bounded, in-memory per-profile read cache (one short
  TTL, clamped 5–300s) so a burst of identical reads (TUI back-navigation, a
  chatty agent) doesn't re-hit NetBox; re-keyed/cleared on profile switch.
- **`mcp/`** — the read-only MCP server (`nbox serve`): stdio plus a loopback
  HTTP transport (OIDC resource-server auth, audit log, per-caller rate limit),
  exposing the same query + view layer as nine tools and `nbox://{kind}/{ref}`
  resources.

## Data flow

```
CLI args ─► lib::run ─► query/search ─► netbox::client ─► NetBox REST
                                             │
                                             └──► NetBox GraphQL (VRF + route-target views, opt-in)
                          │
                          ▼
                  domain view model ─► output::emit (plain | json | csv)
```

The TUI replaces the last step with the ratatui render loop, reusing the same
`domain` view models via `domain::detail`.

## Output contracts

Scriptable JSON is a compatibility surface. The broad `tests/output_flags_tests.rs`
suite checks that every JSON-producing command shares `--fields`, `--raw`, and
`--envelope` behavior. File-backed goldens in `tests/golden/` pin representative
machine-facing shapes (`status`, `search`, detail views) exactly as rendered by
`output::json::render_with`.

When a JSON shape changes intentionally, update the matching golden file in the
same commit. An unexpected golden diff should be treated as a contract review,
not a formatting chore.

## Test Support

Integration-test fixtures live under `tests/support/`. Use those builders and
wiremock helpers for representative NetBox objects, rendered JSON assertions,
and binary command execution instead of cloning payloads into each test file.
This keeps contract tests readable and makes schema/output changes reviewable in
one place.

## Exit codes

Stable contract (also in AGENTS.md): `0` success · `1` generic · `2` usage ·
`3` auth/permission · `4` not found · `5` ambiguous reference.

## Locked decisions

NetBox 4.2+ (polymorphic `scope`) · `reqwest` 0.12 · REST canonical · GraphQL an
opt-in schema-probed per-surface accelerator (VRF + route-target views; search is always
REST — NetBox GraphQL has no `q` equivalent) · `q=`-primary REST search · spawned
TUI commands · centralized API→web URL conversion · tokens never logged.
