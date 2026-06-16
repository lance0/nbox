# Roadmap

This roadmap tracks nbx from skeleton to safe writes. It maps the implementation phases in [DESIGN.md](DESIGN.md) onto release milestones. Items are intentionally read-only first; write support is deliberately deferred.

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
nbx config init
nbx profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbx profile use work
nbx search edge01 --json
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
nbx device edge01
nbx ip 10.44.208.55
nbx prefix 10.44.208.0/24
nbx vlan 208
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
nbx
```

### Phase 4 — Polish
- ☑ Built-in themes (11 ported from xfr in `tui/theme.rs`); cycle (`t`) + palette `theme`, persisted to `[ui].theme` on TUI exit
- ☑ Update notifications (`updates` feature): GitHub check + CLI notice (`src/update.rs`); TUI banner lands in Phase 3
- ☐ Recent objects
- ☑ Friendly, actionable errors (DESIGN §17 "no X matched … Try: nbx search …")
- ☑ Shell completions (bash/zsh/fish/powershell/elvish) — `nbx completions <shell>`
- ☐ Install script
- ☐ Release builds + artifacts (CI itself lands in Phase 1)
- ☐ Homebrew tap

---

## v0.2 — Nested views & first writes

- ☐ Optional read-only **GraphQL** client for nested device detail (one query for device + interfaces + IPs + rack + site)
- ☐ Interface and cable/connection views on the device screen
- ☐ **Safe writes (initial):** `PATCH` engine, minimal diff, before/after preview, confirmation modal
  - ☐ `nbx device <name> set status <value>`
  - ☐ `nbx interface <device> <iface> set description "..."`
- ☐ `changelog_message` support on writes

---

## v0.3 — Broader safe writes

- ☐ `nbx ip <addr> reserve --description "..."`
- ☐ `nbx tag add <type> <name> <tag>`
- ☐ Write workflows surfaced in the TUI edit mode (`e` / `d` / confirm)
- ☐ OPTIONS / OpenAPI schema discovery to validate filters & write capability per NetBox version

---

## Later / under consideration

- ☐ OS keyring token storage
- ☐ Local SQLite cache (`cache` feature) for fast repeat lookups
- ☐ TurboBulk (NetBox Labs) — **only if** revisited post-1.0: capability-detect `/api/plugins/turbobulk/`, export-only (JSONL, no Parquet/arrow dep), opt-in behind a feature flag. It's a proprietary Cloud/Enterprise server plugin (needs NetBox 4.4.7+), so most self-hosted users can't use it, and bulk import/export is a stated non-goal — hence parked here, not planned.
- ☐ Virtualization (VMs) and tenancy detail views
- ☐ VRF-aware IP/prefix navigation

## Explicit non-goals (v0)

Full CRUD for every model · replacing the NetBox web UI · plugin framework · topology diagrams · bulk import/export · custom script runner · approval workflow engine.
