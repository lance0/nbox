# Architecture

nbox keeps a hard line between the **wire layer** (what NetBox returns) and the
**view models** (what nbox renders). The same view models feed the CLI and the
TUI, so output is consistent across both.

## Layers

- **`netbox/`** — NetBox clients and wire types. REST is canonical; GraphQL is an
  opt-in search backend.
  - `client.rs` — auth, paging, timeouts; retries HTTP 429 (`Retry-After` +
    backoff); maps statuses to typed errors (401→auth, 403→perms, 404→not-found).
    The same authenticated client owns `/graphql/` POSTs when a profile selects
    `backend = "graphql"`.
  - `endpoints.rs` — endpoint paths.
  - `pagination.rs` — `Page<T>`, offset paging (`list` / `list_all`).
  - `query.rs` — per-object resolvers (`*_by_ref`, candidates, scope labels).
  - `graphql.rs` — schema capability probing for the GraphQL search backend
    (filter input shapes and pagination support across NetBox 4.2/4.3/4.5+).
  - `search.rs` — parallel `q=` fan-out → ranked, deduped `SearchOutcome`;
    branches to GraphQL only when the active profile asks for it.
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
                                             └──► NetBox GraphQL (search only, opt-in)
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

NetBox 4.2+ (polymorphic `scope`) · `reqwest` 0.12 · REST default with opt-in
schema-probed GraphQL search · `q=`-primary search · spawned TUI commands ·
centralized API→web URL conversion · tokens never logged.
