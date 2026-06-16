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
- ☐ `BriefObject`, `Choice<T>`, `Tag`, custom fields
- ☐ Device, Interface, IPAddress, Prefix, VLAN, Site, Rack
- ☐ Endpoint mapping + per-endpoint query methods
- ☐ Normalized `SearchResult` + parallel multi-endpoint search (`q` primary, field filters fallback)
- ☐ Device / IP / Prefix / VLAN detail resolution (incl. IP → parent prefix via `ipnet`)
- ☐ Plain + JSON output for each command

**Deliverable**

```bash
nbx device edge01
nbx ip 10.44.208.55
nbx prefix 10.44.208.0/24
nbx vlan 208
```

### Phase 3 — TUI v0
- ☐ Terminal init/restore
- ☐ App state + mpsc event loop
- ☐ Search screen + results pane
- ☐ Detail pane (device / ip / prefix / vlan / site)
- ☐ Navigation history (`b` / `Esc`)
- ☐ Help modal
- ☐ Command palette (`:`)
- ☐ Client-side fuzzy ranking (`nucleo`) for the palette + in-memory result lists
- ☐ Open in browser (`o`)
- ☐ Copy to clipboard (`y`)

**Deliverable**

```bash
nbx
```

### Phase 4 — Polish
- ☐ Built-in themes + cycle/persist (`t`)
- ☐ Recent objects
- ☐ Friendly, actionable errors
- ☐ Shell completions (bash/zsh/fish/powershell/elvish)
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
- ☐ Update notifications (`updates` feature)
- ☐ Virtualization (VMs) and tenancy detail views
- ☐ VRF-aware IP/prefix navigation

## Explicit non-goals (v0)

Full CRUD for every model · replacing the NetBox web UI · plugin framework · topology diagrams · bulk import/export · custom script runner · approval workflow engine.
