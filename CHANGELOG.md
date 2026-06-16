# Changelog

All notable changes to nbox are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added (Phase 4 polish, in progress)
- `nbox status` — shows the NetBox URL and version (NetBox/Django/Python from `/api/status/`), plain or `--json`. `Status` gained optional `django`/`python` fields; added `NetBoxClient::base_url()`.
- `nbox prefix` now shows utilization with a small ASCII bar when the NetBox serializer provides it (permissively coerced from number or `"NN%"` string; absent → omitted).
- Planning docs: `RELEASING.md` (crates.io publish + cargo-dist) and an expanded `ROADMAP.md` (IPAM allocation, cable/interface trace, hierarchical prefix tree, scriptable/agent-friendly output, prioritized backlog).
- crates.io metadata (`readme`, `homepage`, richer `description`); `cargo publish --dry-run` is clean.
- Theme persistence: the active theme (cycled with `t` or set via the palette `theme` command) is saved to `[ui].theme` on TUI exit, format-preserving (`config::save_ui_theme`).
- Friendly, actionable errors: not-found lookups now print the DESIGN §17 style — e.g. `no device matched "edge01"` followed by `Try:\n  nbox search edge01` — on stderr.
- Shell completions confirmed wired via `nbox completions <bash|zsh|fish|powershell|elvish>`.

### Fixed
- Unimplemented commands (`interface`, `open`) now exit non-zero instead of reporting success.
- `device`/`rack` lookup by a non-existent numeric ID now returns "not found" (HTTP 404 → `Ok(None)`) instead of surfacing a raw API error; added `NetBoxClient::get_optional`.
- The TUI now actually probes `/api/status/` on launch (`verify_compatible`) — enforcing the 4.2 floor and showing the NetBox version in the header; corrected the `status.rs` doc to match (CLI commands intentionally skip the probe).
- Logging is now initialized (`nbox::init_logging`): `tracing` output goes to stderr, controlled by `--log-level` / `NBOX_LOG` / `RUST_LOG` (quiet by default). Previously `--log-level` was ignored and all `tracing` output was discarded.

### Added
- Initial project design and documentation: `DESIGN.md`, `README.md`, `ROADMAP.md`, `CHANGELOG.md`.
- Project scaffold: `Cargo.toml` (full dependency set, features, release profile), crate skeleton (`src/main.rs`, `src/lib.rs`), dual MIT/Apache-2.0 license files, and GitHub Actions CI (fmt, clippy, build, test).
- CLI skeleton (`src/cli.rs`): full `clap` command surface (`search`, `device`, `ip`, `prefix`, `site`, `rack`, `vlan`, `interface`, `open`, `config`, `profile`, `completions`, `tui`) with global flags; dispatch via `nbox::run`. Shell completion generation is wired; other handlers are stubs that report to stderr (stdout stays clean for piping).
- Authentication (`src/netbox/auth.rs`): `AuthScheme` (`auto`/`bearer`/`token`) with v2-token (`Bearer nbt_…`) auto-detection.
- Configuration (`src/config.rs`): typed `Config`/`UiConfig`/`ProfileConfig`, platform config path, `NBOX_TOKEN`-first token resolution, and format-preserving (`toml_edit`) writes. Implements `config init`/`path`/`show` and `profile add`/`use`/`list`/`show` with `--json` output.
- NetBox REST client (`src/netbox/{client,endpoints,pagination}.rs`): `reqwest` 0.12 client with TLS/timeout from profile, `Endpoint` paths, generic `Page<T>`, `get`/`list`/`list_all` with offset pagination, automatic `exclude=config_context` for devices/VMs, and subpath-safe URL joining. Request logging redacts the token (scheme marker only). Covered by `wiremock` integration tests.
- Version probe (`src/netbox/status.rs`): `/api/status/` fetch and `verify_compatible` enforcing the NetBox 4.2+ floor, with prerelease-tolerant version parsing.
- Output module (`src/output/`): `Format` (plain/json from `--json`), pretty JSON printing, and a `KeyValues` plain-text detail renderer; `config`/`profile` JSON output routed through it. Completes Phase 1 (read-only foundation).
- Themes (`src/tui/theme.rs`): 11 built-in color themes (default, kawaii, cyber, dracula, monochrome, matrix, nord, gruvbox, catppuccin, tokyo_night, solarized) with `by_name`/`list`/`index_of`, ported from xfr. Cycling/persistence wires in with the TUI.
- Update notifications (`src/update.rs`, behind the `updates` feature): background GitHub release check (`update-informer`, pure-Rust TLS) with an install-method-aware CLI notice on stderr (skipped for `--json` and non-TTY). Ported from ttl with xfr's `v`-prefix fix.
- NetBox models (`src/netbox/models/`): permissive wire types — `BriefObject` (with `label()`), `Choice<T>`, `Tag`; DCIM `Device`/`Interface`/`Site`/`Rack`; IPAM `IpAddress`/`Prefix`/`Vlan`/`Vrf`; tenancy `Tenant`. Prefix uses the 4.2+ polymorphic `scope`. Deserialization tests included.
- `nbox device <name|slug|id>`: resolves via id/`name__ie`/`name__ic`, renders a flattened `DeviceView` (`src/domain/`) in plain or `--json`. Dispatch is now async (`#[tokio::main]`); a `connect()` helper builds the client from the active/`--profile` profile. Covered by `wiremock` query tests.
- `nbox ip <address>`: finds the IP (host-aware `address` filter), resolves the most-specific containing prefix locally with `ipnet`, and renders `IpView` (status, DNS, VRF, tenant, assigned object, parent prefix, VLAN/site context) in plain or `--json`.
- `nbox prefix <cidr>`: resolves the exact prefix and renders `PrefixView` (status, scope/site, VRF, VLAN, tenant, role, child count) plus child prefixes (`within`) and contained IP addresses (`parent`, with assigned-object labels), capped at 50 each, in plain or `--json`.
- `nbox vlan <vid|name>`, `nbox site <name|slug>`, `nbox rack <name|id>`: ref resolution (vid/slug/id then `name__ie`/`name__ic`) with `VlanView` (+ referencing prefixes), `SiteView`, and `RackView` in plain or `--json`.
- `nbox search <query> [--limit]`: normalized multi-endpoint search (`src/netbox/search.rs`) — parallel `q=` fan-out across devices/sites/IPs/prefixes/VLANs, merged + ranked (exact/prefix/contains) + deduped, with web URLs via the centralized `util::format::api_to_web_url`. Plain (kind/display/subtitle) or `--json`. **Completes Phase 2.**
- TUI skeleton (`src/tui/{app,state,events,ui}.rs`): `nbox`/`nbox tui` launch a ratatui app (panic-safe init/restore) with a crossterm `EventStream` loop where network commands are **spawned** (never awaited in render). Search screen (`/` → live results, `j`/`k` select), help modal (`?`/`F1`), theme cycling (`t`), themed via `[ui].theme`. Input handling is pure and unit-tested.
- TUI detail + actions: `Enter` opens a result's detail pane (`domain::detail::load_detail`, reusing the CLI view models), `b`/`Esc` navigate back via a screen-history stack, `o` opens the object's web URL in a browser, `y` copies the selected value to the clipboard (`arboard`, behind the `clipboard` feature; graceful message when absent).
- TUI command palette (`:`, `src/tui/palette.rs`): `device`/`ip`/`prefix`/`vlan`/`site <ref>` (open detail), `find <q>` or bare text (search), `open`/`copy`, `theme <name>`, `refresh`; parser is pure and unit-tested.
- TUI fuzzy filtering (`src/tui/fuzzy.rs`, `nucleo`): typing in search mode live-filters and ranks the in-memory results (a `view` index list); server `q=` still does the fetch on Enter. **Completes Phase 3 (TUI v0).**

[Unreleased]: https://github.com/lance0/nbox/commits/master
