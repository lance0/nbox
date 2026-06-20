# Contributing

Thanks for your interest in contributing to nbox!

## Development setup

### Prerequisites

- Rust 1.88+ (edition 2024)
- A NetBox 4.2+ instance to test against (a token with read access)

### Building

```bash
git clone https://github.com/lance0/nbox
cd nbox
cargo build
```

### Running

```bash
# Point at a NetBox instance
cargo run -- config init
cargo run -- profile add dev https://netbox.example.com --token-env NETBOX_TOKEN
export NETBOX_TOKEN=...

cargo run -- status
cargo run -- search edge01
cargo run             # launch the TUI
```

The default build is the full single binary (clipboard, HTTP/MCP, keyring,
updates); `--no-default-features` gives a lean stdio-only build. The only opt-in
is `--features keyring-secret-service` (the Linux D-Bus keyring backend).

## Code style

Standard Rust formatting and linting. `clippy::pedantic` is a true whole-project
gate, enforced via the `[lints]` table in `Cargo.toml` (it reaches the lib, bin,
and every test crate; a handful of pure-noise lints are allowed package-wide
there — prefer fixing over adding to that list). All PRs must pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build
cargo test --all-features
```

### Pre-commit hooks

`.pre-commit-config.yaml` runs `cargo fmt` and `cargo clippy` on commit and
`cargo test` on push. Set it up once:

```bash
# Recommended: prek (fast Rust port, drop-in compatible)
cargo install --locked prek
prek install

# Or the original Python pre-commit
pipx install pre-commit
pre-commit install --hook-type pre-commit --hook-type pre-push
```

## Testing

```bash
cargo test --all-features
cargo test --all-features -- --nocapture
```

The client and query layers are covered by `wiremock` integration tests
(`tests/`); view models and pure logic have unit tests. End-to-end tests that hit
a real NetBox are marked `#[ignore]` and run in CI by the `netbox-integration.yml`
workflow (it boots netbox-docker). To run them locally, bring up the harness in
`tests/integration/` (see its README) and `cargo test -- --ignored`.

## Project structure

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full picture. The key
split: the wire layer (`netbox/`) stays separate from the view models
(`domain/`), and the TUI's input handling is pure.

- `src/netbox/` — REST client, endpoints, pagination, auth, query helpers, models
- `src/domain/` — flattened view models (one per object) shared by CLI and TUI
- `src/output/` — plain / JSON / CSV rendering
- `src/tui/` — ratatui app (pure `handle_event`, spawned network commands)
- `src/config.rs`, `src/error.rs`, `src/cli.rs`, `src/lib.rs`
- `src/cache.rs` — the in-memory read cache; `src/mcp/` — the MCP server

### Adding a new object lookup

The wire→view split makes most additions mechanical. End to end:

1. **Wire model** — add the struct to `src/netbox/models/<group>.rs` (permissive:
   nullable fields, `#[serde(default)]`, unknown fields ignored).
2. **Endpoint + resolver** — add the path to `src/netbox/endpoints.rs` and a
   `<kind>_by_ref` resolver to `src/netbox/query.rs`.
3. **View model** — add `src/domain/<kind>_view.rs` (flattened; owns plain-text
   rendering and `Serialize`), wired into `domain::detail`.
4. **CLI** — add the subcommand in `src/cli.rs` and dispatch it in `src/lib.rs`.
5. **Surfaces** — add it to `search` (if searchable), the TUI Nav rail, the MCP
   `nbox_get` kinds, and the `open` / `journal` resolvers.
6. **Tests + docs** — a `wiremock` test (plus a golden if it has a JSON shape) and
   the kind lists in README, docs/FEATURES.md, and AGENTS.md, kept in sync.

For an output format or a config knob, start from `src/output/` and `src/config.rs`
(the `set_ui_field` + format-preserving `save_ui_field`/`save_ui_fields` setters).

## Pull request process

1. Fork and branch from `master`.
2. Make your changes; keep the wire layer and view models separate.
3. Ensure the checks above pass.
4. Open a PR.

### Commit messages

- Start with a verb (Add, Fix, Update, Remove, Refactor).
- Keep the first line under 72 characters.

Good examples:
- `Add nbox circuit lookup`
- `Fix 404 exit-code mapping on raw GET`
- `Update ratatui to 0.30`

### What to include

- **Bug fixes:** steps to reproduce and verify.
- **New features:** update README.md and the relevant docs.
- **Breaking changes:** note in CHANGELOG.md.

## Reporting issues

Please include:
- OS and version
- Rust version (`rustc --version`)
- nbox version (`nbox --version`)
- NetBox version (`nbox status`)
- Steps to reproduce, expected vs actual, and any error messages

## License

By contributing, you agree your contributions are licensed under the project's
dual MIT/Apache-2.0 license.
