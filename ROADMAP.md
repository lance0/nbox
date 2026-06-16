# Roadmap

This roadmap tracks nbox from skeleton to safe writes. It maps the implementation phases in [DESIGN.md](DESIGN.md) onto release milestones. Items are intentionally read-only first; write support is deliberately deferred.

Legend: ☐ planned · ◐ in progress · ☑ done

## Principles

- **Agent-first.** The CLI, TUI, and `nbox mcp serve` all run off the same command core. JSON/envelope/`--fields`/`--raw` and `AGENTS.md` exist now; MCP lands in v0.2/v0.3.
- **Read-only first.** Reads ship before writes. Writes are `PATCH`-based, diff-previewed, confirmable, and opt-in over MCP.
- **Correctness over breadth.** Typed errors and CI against a real NetBox before more surface area.

---

## v0.1 — Read-only foundation

The goal of v0.1 is a working vertical slice: configure a profile, search, look up objects from the shell, and navigate them in the TUI.

### Phase 1 — Skeleton
- ☑ `Cargo.toml` metadata + dependencies
- ☑ Dual MIT/Apache license files
- ☑ `clap` CLI skeleton with global flags (`--profile`, `--config`, `--json`, `--no-tui`, `--log-level`)
- ☑ Config loader + `config init` / `config path` / `config show`
- ☑ Profile commands (`add` / `use` / `list` / `show`)
- ☑ Auth header support: `auto` / `bearer` / `token`
- ☑ `reqwest` 0.12 client with TLS + timeout settings
- ☑ Token redaction in request logging (never log `Authorization`)
- ☑ Paginated `Page<T>` + `list` / `list_all`
- ☑ `/api/status/` version probe + 4.2 floor enforcement (`verify_compatible`); status-line display lands with the TUI (Phase 3)
- ☑ JSON output path
- ☑ CI green from commit 1 (fmt, clippy, test on GitHub Actions)

**Deliverable**

```bash
nbox config init
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
nbox search edge01 --json
```

### Phase 2 — Core REST models
- ☑ `BriefObject`, `Choice<T>`, `Tag`, custom fields
- ☑ Device, Interface, IPAddress, Prefix, VLAN, Site, Rack (+ Vrf, Tenant)
- ☑ Endpoint mapping + per-endpoint query methods (device/ip/prefix/vlan/site/rack)
- ☑ Normalized `SearchResult` + parallel multi-endpoint search (`q` primary across devices/sites/ips/prefixes/vlans)
- ☑ Device / IP / Prefix / VLAN / Site / Rack detail resolution (incl. IP → parent prefix via `ipnet`)
- ☑ Plain + JSON output for each detail command

**Deliverable**

```bash
nbox device edge01
nbox ip 10.44.208.55
nbox prefix 10.44.208.0/24
nbox vlan 208
```

### Phase 3 — TUI v0
- ☑ Terminal init/restore (panic-safe via `ratatui::init`)
- ☑ App state + mpsc event loop (crossterm `EventStream`, spawned commands)
- ☑ Search screen + results pane (`/` → live search, j/k select)
- ☑ Detail pane (device / ip / prefix / vlan / site) — Enter loads via `domain::detail::load_detail`
- ☑ Navigation history (`b` / `Esc`, screen stack)
- ☑ Help modal (`?`/`F1`)
- ☑ Command palette (`:`) — `device`/`ip`/`prefix`/`vlan`/`site`/`find`/`open`/`copy`/`theme`/`refresh`
- ☑ Client-side fuzzy ranking (`nucleo`) — live filtering of in-memory results while typing
- ☑ Open in browser (`o`, via `open` + `util::format::api_to_web_url`)
- ☑ Copy to clipboard (`y`, `arboard` behind the `clipboard` feature)

**Deliverable**

```bash
nbox
```

### Phase 4 — Polish & release

Done / carried:
- ☑ Built-in themes (11 in `tui/theme.rs`); cycle (`t`) + palette `theme`, persisted to `[ui].theme` on TUI exit
- ☑ Update notifications (`updates` feature): GitHub check + CLI notice (`src/update.rs`); TUI banner lands in Phase 3
- ☑ Friendly, actionable errors (DESIGN §17 "no X matched … Try: nbox search …")
- ☑ Shell completions (bash/zsh/fish/powershell/elvish) — `nbox completions <shell>`
- ☑ Recent objects (TUI: capped/deduped, most-recent-first; shown on Home when there are no results; Enter reopens)

Release & distribution (v0.1 release gate):
- ☑ Release pipeline: hand-written `.github/workflows/release.yml` on tag `v*` — matrix build (Linux x86_64/aarch64, macOS Intel/ARM, Windows) → archives + `.sha256` to the GitHub Release (plain workflow over cargo-dist to avoid a mid-CI install)
- ☑ Install script (`scripts/install.sh`: detect OS/arch, download latest release asset, `cargo install` fallback)
- ☑ Homebrew tap formula template (`packaging/homebrew/nbox.rb`; needs a tap repo + real URLs/sha256 at release time)
- ☑ Publish to crates.io — `nbox` 0.1.0 published (name camped; next release 0.1.1+)
- ☑ README pass: install (crates.io/install.sh/Homebrew), all commands + global flags, TUI keybindings (palette/themes/recent/auto-refresh); demo recording placeholder pending the release

Feature wins (small, on-identity):
- ☑ `nbox status` — connection + NetBox/Django/Python versions (plain + `--json`)
- ☑ Prefix utilization in `nbox prefix` output (NetBox `utilization` %, with a small bar; permissive — shown only when present)
- ☑ Custom fields in detail output (`cf.<name>` rows + JSON, non-null, across device/ip/prefix/vlan/site/rack)
- ☑ Structured filter flags on `search`: `--status`/`--site`/`--tenant`/`--role` (per-endpoint allowlist; unsupported→endpoint skipped). `--vrf` deferred (needs name→RD/id resolution; with filter validation in v0.2)
- ☑ CSV output: global `-o/--output plain|json|csv` (`--json` is a shortcut); generic (arrays→table, objects→field,value)
- ☑ Column selection `--cols a,b,c` for `search` CSV output
- ☑ Auto-refresh tick in the TUI (`[ui].refresh_secs`, default off; re-runs the last query, preserving the cursor by id)
- ☑ Client-side filter validation — structurally avoided: exposed filters are typed + per-endpoint allowlisted, so nbox never sends unknown params (netbox#6489). Value-level validation → v0.3 OPTIONS/schema discovery.

Scriptable / agent-friendly output:
- ☑ Versioned JSON envelope (`--envelope` → `{ schema_version, data }`) + stable exit codes (structured JSON errors deferred)
- ☑ `--fields a,b,c` / `--raw` output controls
- ◐ `AGENTS.md` added; per-command skill files + a `--dry-run` convention land with writes (v0.2)

---

## Prioritized backlog

| # | Feature | Lands |
|---|---|---|
| 1 | `next-ip` / `next-prefix` (available IPs/prefixes) | v0.2 (with safe writes) |
| 2 | `nbox status` | v0.1 Phase 4 |
| 3 | Structured filter flags (`--status`/`--site`/…) | v0.1 Phase 4 |
| 4 | Prefix utilization in output | v0.1 Phase 4 |
| 5 | Cable/interface trace (`/interfaces/{id}/trace/`) | v0.2 (interface detail) |
| 6 | Custom fields in detail output | v0.1 Phase 4 |
| 7 | Column selection (`--cols`) | v0.1 Phase 4 |
| 8 | CSV output | v0.1 Phase 4 |
| 9 | Hierarchical prefix tree in TUI | v0.2 |
| 10 | Auto-refresh tick in TUI | v0.1 Phase 4 |

---

## v0.1.1 — Close the gap

v0.1 documents `open`, `interface`, and the TUI device tabs but doesn't implement them. Ship them or drop them from the docs, and pull the cheap read-only wins forward.

- ☑ `nbox open` — web URL via `util::format::api_to_web_url` + `open`.
- ☑ `nbox interface <device> <iface>` — flat view plus its addresses.
- ☑ TUI device tabs: `i` interfaces · `p` IPs · `c` cables · `v` VLANs. `nbox device` also shows the full set.
- ☐ Read-only `nbox next-ip <prefix>` / `next-prefix <prefix>` via `available-ips` / `available-prefixes`. Allocate lands with writes (v0.2).
- ☑ Typed errors (`src/error.rs`) — 401→auth, 403→perms, ambiguous name→list matches; stable exit codes (3 auth, 4 not-found, 5 ambiguous).
- ☐ CI against a real NetBox — netbox-docker (pin 4.x ≥ 4.2), seeded fixture + legacy v1 token, run the binary against the live API. Catches serializer drift wiremock can't.
- ☐ Read-only `nbox raw GET <path>`.
- ☐ `config_version` field + forward-compat, before v0.2 touches the schema.
- ☐ `clap_mangen` man pages.
- ☐ Mark absent DESIGN.md modules aspirational (`error.rs`, `graphql.rs`, `schema.rs`, `docs/`).

## v0.2 — Nested views, IPAM power, first writes

- ☐ **MCP server (read-only): `nbox mcp serve`** — command core as MCP tools: search, device, ip, prefix, vlan, site, status, next-ip/next-prefix. stdio first, HTTP later.
- ☐ **Large-instance robustness** — honor 429 `Retry-After`, bound search/`list_all` concurrency, cap paging.
- ☐ **IPAM allocate (write)** — claim the next IP/prefix (POST to `available-ips`/`available-prefixes`). Read-only half is v0.1.1.
- ☐ **Cable / interface trace** — `/api/dcim/interfaces/{id}/trace/`; surface on the interface/device view.
- ☐ **Hierarchical prefix tree in the TUI** — expand/collapse children with inline utilization (netbox#21396/#21255).
- ☐ **Device detail — pick one path** — REST fan-out (device + interfaces + IPs) or a read-only GraphQL query. Don't build both.
- ☐ Multi-pane TUI (nav | results | detail) per the DESIGN mockup.
- ☐ IP ranges (`/api/ipam/ip-ranges/` + `available-ips`).
- ☐ **Safe writes (initial)** — `PATCH` engine, minimal diff, before/after preview, confirmation modal; agent-safe `--read-only` profile.
  - ☐ Settle write rules first: choice fields (`{value,label}`→string), brief relations (slug/id/name), confirmation in non-TTY/`--json`/MCP.
  - ☐ `nbox device <name> set status <value>`
  - ☐ `nbox interface <device> <iface> set description "..."`
- ☐ `changelog_message` support on writes.

---

## v0.3 — Broader models, writes, discovery

- ☐ **Write-capable MCP tools**, opt-in (`--allow-writes` / a write profile) — return the diff for the agent to confirm. Read-only stays default.
- ☐ `nbox ip <addr> reserve --description "..."`
- ☐ `nbox tag add <type> <name> <tag>`; tag browsing (`nbox tags`, `--tag <name>` filter).
- ☐ Write workflows in the TUI edit mode (`e` / `d` / confirm).
- ☐ **`--vrf` resolution** — accept id | rd (`65000:100`) | name, that precedence; ambiguous name → list matches. Also fixes first-match-wins in `ip_candidates`.
- ☐ Circuits (`nbox circuit <id>`, included in search).
- ☐ Aggregates (`/api/ipam/aggregates/`) and ASNs (`/api/ipam/asns/`).
- ☐ Journal entries on detail views (`/api/extras/journal-entries/`).
- ☐ Services (`/api/ipam/services/`) — what's listening on this device.
- ☐ `nbox raw POST|PATCH|DELETE <path>` (read-only GET ships in v0.1.1).
- ☐ OPTIONS write-capability discovery — filter safety is already handled by the typed allowlist; optional `schema` command.
- ☐ Batch queries from a file (audits).

---

## Later / under consideration

- ☐ Dashboard / overview screen (counts by status, utilization, recent changes).
- ☐ Context preservation in the TUI (scroll position + filters per view).
- ☐ OS keyring token storage.
- ☐ Virtualization (VMs) and tenancy detail views.
- ☐ VRF-aware IP/prefix navigation (built on the v0.3 `--vrf` resolution).
- ☐ TurboBulk export — capability-detect `/api/plugins/turbobulk/`, read/export-only (JSONL, no arrow/parquet dep), behind a feature flag, clean fallback when absent. Fast full-table export/audit on large instances where paginated REST is too slow.

**Reconsidering / likely cut**
- Local SQLite cache (`cache` feature) — the value here is freshness, and `nucleo` already covers interactive speed. Adds a bundled-C dep and invalidation complexity. Parked unless a real large-instance latency problem shows up; dead weight today.
- Plugin / custom-command system — cut; it's a non-goal.

## Infrastructure & quality

Ported from ttl/xfr where they paid off. Already have: release workflow, `install.sh`, Homebrew template, completions, MSRV, keep-a-changelog. Themes and the update-notifier are already in.

- ☐ `cargo-audit` CI (`audit.yml`) — on Cargo.toml/lock + daily cron.
- ☐ Pre-commit hooks (`.pre-commit-config.yaml`) — fmt/clippy on commit, test on push; prek with a Python fallback.
- ☐ musl Linux targets in the release matrix (static binaries). gnu only today.
- ☐ `Dockerfile.release` + multi-arch GHCR image (`ghcr.io/lance0/nbox`).
- ☐ Ship completions as a release artifact, not just the subcommand.
- ☐ MSRV CI job pinning `rust-version` (1.88).
- ☐ `dependabot.yml` — grouped Cargo + GitHub Actions.
- ☐ `CONTRIBUTING.md`.
- ☐ `docs/` tree — `ARCHITECTURE.md`, `CONFIG.md`, `FEATURES.md`.
- ☐ `KNOWN_ISSUES.md` — first-match disambiguation, no VRF scoping yet, etc.
- ☐ Split `prefs.toml` (runtime state) from `config.toml` (user config), per xfr. Pairs with `config_version`.
- ☐ `examples/config.toml`; `.github/FUNDING.yml`.

## Explicit non-goals (v0)

Full CRUD for every model · replacing the NetBox web UI · plugin framework · topology diagrams · a bulk import/export engine (TurboBulk export aside) · custom script runner · approval workflow engine.
