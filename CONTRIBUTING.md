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

Optional features: `--features cache,updates` (clipboard is on by default).

## Code style

Standard Rust formatting and linting. All PRs must pass:

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
(`tests/`); view models and pure logic have unit tests. There are no live-NetBox
tests yet — that CI lands separately (see ROADMAP).

## Project structure

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full picture. The key
split: the wire layer (`netbox/`) stays separate from the view models
(`domain/`), and the TUI's input handling is pure.

- `src/netbox/` — REST client, endpoints, pagination, auth, query helpers, models
- `src/domain/` — flattened view models (one per object) shared by CLI and TUI
- `src/output/` — plain / JSON / CSV rendering
- `src/tui/` — ratatui app (pure `handle_event`, spawned network commands)
- `src/config.rs`, `src/error.rs`, `src/cli.rs`, `src/lib.rs`

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
