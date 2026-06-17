# Changelog

All notable changes to nbox are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-06-17

The first real release. (`0.1.0` was a name reservation on crates.io.)

### Changed (BREAKING)
- Polymorphic scope (NetBox 4.2+) is now surfaced across the IPAM views. **BREAKING:** the `ip` view's `site` field is renamed to `scope`; prefixes and VLANs now surface non-site scopes (location, region, site-group, …) instead of only the site case, and all three views gain a `scope_type` field (a friendly label: `site`/`location`/`region`/`site-group`, or the raw content type for anything else). `scope` holds the scope object's name for any scope type; `ip` derives both from its most-specific parent prefix. No `site` field remains on the `ip`/`prefix`/`vlan` views — consumers must read `scope`/`scope_type`.

### Added (lookups, IPAM & TUI)
- Read-only IPAM allocation: `nbox next-ip <prefix>` (next available address(es), `--count`) and `nbox next-prefix <prefix>` (free child blocks, or the first of `--length`, computed locally with `ipnet`). Both take `--vrf` to scope the prefix. Via the NetBox `available-ips`/`available-prefixes` endpoints.
- `nbox open <kind>/<ref>` — resolve a device/site/rack/vlan/prefix/ip to its web URL and open it (was a stub).
- `nbox raw GET <path>` — raw read-only API request for unmodeled endpoints; prints the JSON body (honors `-o`/`--fields`/`--raw`/`--envelope`). Write verbs are rejected until safe writes land (v0.2+).
- `config_version` field (`config init` writes `1`). Loading a config with a newer version warns but still works; an absent version is treated as v1. Forward-compat groundwork before v0.2 changes the schema.
- `nbox man` generates a roff man page (`nbox man > nbox.1`) via `clap_mangen`, alongside the existing shell completions.
- `nbox interface <device> <iface>` — interface detail (type, MTU, MAC, mode, untagged/tagged VLANs, cable, connected endpoints, addresses), plain or `--json` (was a stub).
- `nbox device` now includes the device's interfaces, IP addresses, cables, and VLANs; the TUI device screen gains `i`/`p`/`c`/`v` tabs for the same.
- Typed errors (`src/error.rs`) with stable exit codes: `3` auth/permission (HTTP 401/403), `4` not found, `5` ambiguous reference, `1` other. Name-contains lookups that match more than one object now report the candidates instead of silently taking the first. Documented in `AGENTS.md`.

### Added (read coverage)
- `nbox serve` — read-only MCP server over stdio (`rmcp` 1.7), exposing the CLI's lookups as eight read-only-annotated tools: `nbox_status`, `nbox_search`, `nbox_get`, `nbox_get_interface`, `nbox_next_ip`, `nbox_next_prefix`, `nbox_journal`, `nbox_list_tags`. An MCP host launches it as a subprocess and speaks JSON-RPC over stdin/stdout; the tools return the same JSON view models as the CLI. URL/token come from the active profile (same `-p`/`--config` flags); JSON-RPC on stdout, logs on stderr. HTTP transport, OAuth, a raw escape-hatch tool, and MCP resources/prompts are later.
- Precise per-tool output schemas for the MCP server. The seven type-stable tools (`nbox_status`, `nbox_search`, `nbox_get_interface`, `nbox_next_ip`, `nbox_next_prefix`, `nbox_journal`, `nbox_list_tags`) now return their concrete view types so `rmcp` derives a real `outputSchema` from `schemars`, instead of the permissive `{"type":"object"}`. `nbox_get` keeps the permissive schema (its shape is polymorphic by kind). Serialized JSON is unchanged.
- `nbox circuit <cid|id>` — look up a circuit by CID (exact, then contains) or numeric ID, rendering provider, type, status, tenant, commit rate, and custom fields (plain or `--json`). Ambiguous CID prefixes exit 5.
- `nbox aggregate <cidr|id>` — look up an aggregate by CIDR or numeric ID (RIR, tenant, date added, custom fields).
- `nbox asn <asn>` — look up an ASN by number (RIR, tenant, custom fields).
- Services on the device detail — `nbox device` now includes a services section (name, protocol, ports), and the TUI device screen gains an `s` tab ("what's listening").
- `nbox ip-range <start|id>` — look up an IP range by start address or numeric ID (start/end, size, status, VRF, tenant, role, custom fields).
- `nbox journal <kind> <ref>` — list recent journal entries (created, kind, author, comments) for a device/ip/prefix/vlan/site/rack/circuit/aggregate/asn/ip-range, newest first.
- `--journal` on the detail commands (device/ip/prefix/vlan/site/rack/circuit/aggregate/asn/ip-range) folds an object's most recent journal entries into its lookup — a top-level `journal` array (`--json`) or a Journal section (plain). Without the flag, output is byte-identical to before.
- `nbox search` now also covers circuits, aggregates, ASNs, and IP ranges (same `q=` quick-search + supported filters as the other endpoints); ASNs additionally match a purely numeric query against the `asn` field.
- `nbox tags` lists tags (slug, name, count); `nbox search --tag <slug>` filters by tag on the endpoints that support it (skipping those that don't, like the other structured filters).
- `nbox interface` now shows a Cable Path section, tracing the cabled path (`/interfaces/{id}/trace/`) hop by hop (`near --[cable]-- far`).

### Changed (robustness)
- The REST client now retries on HTTP 429 (rate limited), honoring `Retry-After` (capped at 60s) with exponential backoff, up to 3 attempts — so large/throttled instances don't fail a lookup on a transient 429.

### Changed / fixed (correctness)
- `nbox search --site <ref>` now resolves the site once up front and errors (exit 4) on an unknown site, instead of silently returning no results. It also filters prefixes by site scope (`scope_type=dcim.site` + the resolved `scope_id`), since NetBox 4.2+ replaced the prefix `site` FK with the polymorphic `scope` and a plain `?site=` is a dead filter there. (Site-scope only — region/site-group/location scope filtering is deferred.)
- `nbox ip` parent-prefix enrichment is now VRF-scoped: `prefixes_containing` filters by the resolved IP's VRF (`vrf_id`, or `null`/global when the IP has none), so the reported `parent_prefix` (and the VLAN/site derived from it) can't come from a different VRF with overlapping space.
- HTTP 404 now maps to the not-found exit code (4) on every path, including a raw `get`/`nbox raw GET …/999999/`. Previously a 404 outside the by-ID `get_optional` path fell through to a generic error (exit 1), so the same condition could exit 1 or 4 depending on the route.
- Reference disambiguation across scopes. NetBox allows duplicate IPs/prefixes across VRFs and duplicate VLAN IDs across sites/groups. `nbox ip`/`prefix`/`vlan` now error (exit 5) listing the candidates when a reference matches several, instead of silently returning the first. Added `--vrf` (ip/prefix) and `--site`/`--group` (vlan) to scope the match.
- Global output flags are now truly global. `config show`/`path` and `profile list`/`show` route through the same `emit`/`JsonOptions` path as every other command, so `-o csv`, `--fields`, `--raw`, and `--envelope` apply there too (previously they only honored a plain `--json`).
- `search` fails closed by default. If any endpoint errors (e.g. a permission failure), the command now errors instead of presenting partial results as complete; `--partial` opts into best-effort results (with a stderr warning), and the TUI status line shows when results are partial.

### Added (release & distribution)
- Release pipeline (`.github/workflows/release.yml`): on a `v*` tag, a `cargo-audit`-gated matrix build (Linux x86_64/aarch64 musl + aarch64-gnu, macOS Intel/ARM, Windows) produces the per-target archives, a `nbox-completions.tar.gz` + man page, a multi-arch GHCR image (`ghcr.io/lance0/nbox`), and a combined `SHA256SUMS`, uploaded to the GitHub Release. Hand-written (no cargo-dist) to avoid a network install mid-CI.
- `scripts/install.sh`: detects OS/arch, downloads the matching latest-release asset to `~/.local/bin` (or `NBOX_INSTALL_DIR`), and falls back to `cargo install nbox`.
- Homebrew formula template (`packaging/homebrew/nbox.rb`) for a future tap, with per-arch URL/sha256 placeholders and completion generation.
- README pass: crates.io/install-script/Homebrew install paths, full command list, a global-flags table (`-o/--output`, `--json/--raw/--envelope/--fields`, `--profile`, `--config`, `--log-level`, `--no-tui`), expanded TUI/palette/recent/auto-refresh docs, and an asciinema/VHS demo placeholder.

### Added (polish)
- `nbox status` — shows the NetBox URL and version (NetBox/Django/Python from `/api/status/`), plain or `--json`. `Status` gained optional `django`/`python` fields; added `NetBoxClient::base_url()`.
- `nbox prefix` now shows utilization with a small ASCII bar when the NetBox serializer provides it (permissively coerced from number or `"NN%"` string; absent → omitted).
- Custom fields surfaced in all detail views (`device`/`ip`/`prefix`/`vlan`/`site`/`rack`) as `cf.<name>` rows (plain) and a `custom_fields` object (`--json`); null/empty values dropped (`src/domain/custom.rs`).
- `nbox search` structured filters: `--status`/`--site`/`--tenant`/`--role`, mapped to NetBox query params per endpoint. Endpoints that don't support an active filter are skipped (rather than returning everything via NetBox's silent-ignore). `--vrf` deferred pending name resolution.
- Output formats: global `-o/--output plain|json|csv` (`--json` is a shortcut) across all data commands. CSV is generic (`src/output/csv.rs`, RFC 4180-ish): arrays → a table, single objects → `field,value`. `nbox search --cols a,b,c` selects/orders CSV columns.
- TUI auto-refresh: `[ui].refresh_secs` (default off) re-runs the last search on an interval while idle on the home screen, preserving the selected row by id.
- TUI recent objects: opening a detail records it (deduped, most-recent-first, capped at 20); the home screen lists recents when there are no search results, and Enter reopens. `DetailView` now carries the object's kind/id.
- Agent-friendly JSON: `--envelope` wraps output as `{schema_version, data}`, `--fields a,b,c` keeps only those top-level fields, `--raw` emits compact JSON. Added `AGENTS.md` describing the machine-readable surface. (Client-side filter validation is structurally handled by the typed per-endpoint allowlist.)
- Planning docs: `RELEASING.md` (release runbook) and an expanded `ROADMAP.md` (IPAM allocation, cable/interface trace, hierarchical prefix tree, scriptable/agent-friendly output, prioritized backlog).
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

[0.1.1]: https://github.com/lance0/nbox/releases/tag/v0.1.1
