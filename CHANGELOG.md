# Changelog

All notable changes to nbx are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial project design and documentation: `DESIGN.md`, `README.md`, `ROADMAP.md`, `CHANGELOG.md`.
- Project scaffold: `Cargo.toml` (full dependency set, features, release profile), crate skeleton (`src/main.rs`, `src/lib.rs`), dual MIT/Apache-2.0 license files, and GitHub Actions CI (fmt, clippy, build, test).
- CLI skeleton (`src/cli.rs`): full `clap` command surface (`search`, `device`, `ip`, `prefix`, `site`, `rack`, `vlan`, `interface`, `open`, `config`, `profile`, `completions`, `tui`) with global flags; dispatch via `nbx::run`. Shell completion generation is wired; other handlers are stubs that report to stderr (stdout stays clean for piping).
- Authentication (`src/netbox/auth.rs`): `AuthScheme` (`auto`/`bearer`/`token`) with v2-token (`Bearer nbt_…`) auto-detection.
- Configuration (`src/config.rs`): typed `Config`/`UiConfig`/`ProfileConfig`, platform config path, `NBX_TOKEN`-first token resolution, and format-preserving (`toml_edit`) writes. Implements `config init`/`path`/`show` and `profile add`/`use`/`list`/`show` with `--json` output.
- NetBox REST client (`src/netbox/{client,endpoints,pagination}.rs`): `reqwest` 0.12 client with TLS/timeout from profile, `Endpoint` paths, generic `Page<T>`, `get`/`list`/`list_all` with offset pagination, automatic `exclude=config_context` for devices/VMs, and subpath-safe URL joining. Request logging redacts the token (scheme marker only). Covered by `wiremock` integration tests.
- Version probe (`src/netbox/status.rs`): `/api/status/` fetch and `verify_compatible` enforcing the NetBox 4.2+ floor, with prerelease-tolerant version parsing.
- Output module (`src/output/`): `Format` (plain/json from `--json`), pretty JSON printing, and a `KeyValues` plain-text detail renderer; `config`/`profile` JSON output routed through it. Completes Phase 1 (read-only foundation).
- Themes (`src/tui/theme.rs`): 11 built-in color themes (default, kawaii, cyber, dracula, monochrome, matrix, nord, gruvbox, catppuccin, tokyo_night, solarized) with `by_name`/`list`/`index_of`, ported from xfr. Cycling/persistence wires in with the TUI.
- Update notifications (`src/update.rs`, behind the `updates` feature): background GitHub release check (`update-informer`, pure-Rust TLS) with an install-method-aware CLI notice on stderr (skipped for `--json` and non-TTY). Ported from ttl with xfr's `v`-prefix fix.

[Unreleased]: https://github.com/lance0/nbx/commits/master
