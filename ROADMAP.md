# Roadmap

This roadmap tracks nbox from skeleton to safe writes. It maps the implementation phases in [DESIGN.md](DESIGN.md) onto release milestones. Items are intentionally read-only first; write support is deliberately deferred.

Legend: ☐ planned · ◐ in progress · ☑ done

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

### Phase 4 — Polish

**Strategy:** see [docs/POSITIONING.md](docs/POSITIONING.md). The competitive read says **distribution is the #1 throttle** — the release/install items below are the v0.1 launch gate; the small high-value feature wins ride along.

Done / carried:
- ☑ Built-in themes (11 ported from xfr in `tui/theme.rs`); cycle (`t`) + palette `theme`, persisted to `[ui].theme` on TUI exit
- ☑ Update notifications (`updates` feature): GitHub check + CLI notice (`src/update.rs`); TUI banner lands in Phase 3
- ☑ Friendly, actionable errors (DESIGN §17 "no X matched … Try: nbox search …")
- ☑ Shell completions (bash/zsh/fish/powershell/elvish) — `nbox completions <shell>`
- ☐ Recent objects

Launch gate (distribution + first impression — highest leverage):
- ☐ Release pipeline via `cargo-dist`: GitHub Release binaries (macOS Intel/ARM, Linux x86_64/aarch64, Windows) + SHA256SUMS, completions bundled
- ☐ Install script (`scripts/install.sh`: download latest release, `cargo install`/`cargo binstall` fallback)
- ☐ Homebrew tap formula
- ☐ Publish + reserve `nbox` on crates.io (see [RELEASING.md](RELEASING.md))
- ☐ Demo-first README: VHS hero GIF (`/edge01` → keyboard drill-down, speed visible) + 3 short clips (IP→prefix→VLAN, `--json | jq`, `o`/`y`)
- ☐ Quantified speed claim (ripgrep-style: NetBox UI vs `nbox` lookup, screenshot)

High-value feature wins for v0.1 (small, on-identity — from competitive analysis):
- ☐ `nbox status` — expose the existing `/api/status/` probe (version/python/workers) as a command (trivial; reuses `verify_compatible`/`Status`)
- ☐ Prefix utilization in `nbox prefix` output (surface NetBox's `utilization` %, with a small bar)
- ☐ Custom fields in detail output (view models currently drop `custom_fields`; surface `cf_*`)
- ☐ Structured filter flags on `search`/lookups: `--status`, `--site`, `--vrf`, `--tenant`, `--role` (→ API query params)
- ☐ Column selection `--cols id,name,status,site` for plain/table output
- ☐ CSV output (extend `output/`: `--output plain|json|csv`; YAML/jsonl optional)
- ☐ Auto-refresh tick in the TUI (emit the existing `Tick`; configurable interval, k9s-style)
- ☐ Client-side filter validation — reject/warn on unknown query params (NetBox silently returns *everything*; netbox#6489)

Agent parity (table-stakes; cheap; neutralizes the competitor's only claim):
- ☐ Versioned JSON output envelope (`{ schema_version, data }`) + semantic exit codes + structured JSON errors
- ☐ `--fields a,b,c` / `--raw` token-efficiency controls
- ☐ `AGENTS.md` + per-command skill files; a `--dry-run` convention (real once writes land)

---

## Prioritized adds (from competitive analysis, 2026-06-16)

Ranking compiled pre-rename; see [docs/POSITIONING.md](docs/POSITIONING.md) for rationale.

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
| 9 | Hierarchical prefix tree in TUI (signature) | v0.2 |
| 10 | Auto-refresh tick in TUI | v0.1 Phase 4 |

---

## v0.2 — Nested views, IPAM power, first writes

- ☐ **IPAM allocation:** `nbox next-ip <prefix>` / `nbox next-prefix <prefix>` via `/api/ipam/prefixes/{id}/available-ips/` + `/available-prefixes/` (the most-requested NetBox workflow; netbox#66 open since 2016)
- ☐ **Cable / interface trace:** `/api/dcim/interfaces/{id}/trace/` — "where is this port cabled to?"; surface on the interface/device detail view
- ☐ **Hierarchical prefix tree in the TUI (signature feature):** expand/collapse child prefixes with inline utilization — uniquely tractable in a terminal (netbox#21396/#21255)
- ☐ Optional read-only **GraphQL** client for nested device detail (one query: device + interfaces + IPs + rack + site)
- ☐ Interface and cable/connection views on the device screen
- ☐ Multi-pane TUI (nav | results | detail) per DESIGN mockup, vs current screen-switching
- ☐ IP ranges (`/api/ipam/ip-ranges/` + `available-ips`)
- ☐ **Safe writes (initial):** `PATCH` engine, minimal diff, before/after preview, confirmation modal; agent-safe `--read-only` profile
  - ☐ `nbox device <name> set status <value>`
  - ☐ `nbox interface <device> <iface> set description "..."`
- ☐ `changelog_message` support on writes

---

## v0.3 — Broader models, writes, discovery

- ☐ `nbox ip <addr> reserve --description "..."`
- ☐ `nbox tag add <type> <name> <tag>`; tag browsing (`nbox tags`, `--tag <name>` filter)
- ☐ Write workflows surfaced in the TUI edit mode (`e` / `d` / confirm)
- ☐ Circuits (`nbox circuit <id>`, included in search)
- ☐ Aggregates (`/api/ipam/aggregates/`) and ASNs (`/api/ipam/asns/`)
- ☐ Journal entries on detail views (`/api/extras/journal-entries/`)
- ☐ Services (`/api/ipam/services/`) — "what's listening on this device?"
- ☐ `nbox raw <GET|POST|PATCH|DELETE> <path>` escape hatch
- ☐ OPTIONS / OpenAPI schema discovery to validate filters & write capability per NetBox version (also a user-facing `schema` command)
- ☐ Batch queries from a file (audits)

---

## Later / under consideration

- ☐ `nbox mcp serve` (stdio + HTTP) reusing the command core — self-hosted, low-token, **write-safe** complement to NetBox Labs' read-only MCP server (post-1.0; be protocol-agnostic, don't bet the product on it)
- ☐ Dashboard / overview screen (counts by status, utilization stats, recent changes — k9s "pulse")
- ☐ Plugin / custom-command system (`~/.config/nbox/commands.toml`, k9s-style key bindings)
- ☐ Context preservation in the TUI (scroll position + filters per view)
- ☐ OS keyring token storage
- ☐ Local SQLite cache (`cache` feature) for fast repeat lookups
- ☐ TurboBulk (NetBox Labs) — **only if** revisited post-1.0: capability-detect `/api/plugins/turbobulk/`, export-only (JSONL, no Parquet/arrow dep), opt-in behind a feature flag. It's a proprietary Cloud/Enterprise server plugin (needs NetBox 4.4.7+), so most self-hosted users can't use it, and bulk import/export is a stated non-goal — hence parked here, not planned.
- ☐ Virtualization (VMs) and tenancy detail views
- ☐ VRF-aware IP/prefix navigation

## Explicit non-goals (v0)

Full CRUD for every model · replacing the NetBox web UI · plugin framework · topology diagrams · bulk import/export · custom script runner · approval workflow engine.
