# Architecture

nbox keeps a hard line between the **wire layer** (what NetBox returns) and the
**view models** (what nbox renders). The same view models feed the CLI and the
TUI, so output is consistent across both.

## Layers

- **`netbox/`** — the REST client and wire types.
  - `client.rs` — auth, paging, timeouts; retries HTTP 429 (`Retry-After` +
    backoff); maps statuses to typed errors (401→auth, 403→perms, 404→not-found).
  - `endpoints.rs` — endpoint paths.
  - `pagination.rs` — `Page<T>`, offset paging (`list` / `list_all`).
  - `query.rs` — per-object resolvers (`*_by_ref`, candidates, scope labels).
  - `search.rs` — parallel `q=` fan-out → ranked, deduped `SearchOutcome`.
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

## Data flow

```
CLI args ─► lib::run ─► query/search ─► netbox::client ─► NetBox REST
                          │
                          ▼
                  domain view model ─► output::emit (plain | json | csv)
```

The TUI replaces the last step with the ratatui render loop, reusing the same
`domain` view models via `domain::detail`.

## Exit codes

Stable contract (also in AGENTS.md): `0` success · `1` generic · `2` usage ·
`3` auth/permission · `4` not found · `5` ambiguous reference.

## Locked decisions

NetBox 4.2+ (polymorphic `scope`) · `reqwest` 0.12 · `q=`-primary search ·
spawned TUI commands · centralized API→web URL conversion · tokens never logged.
