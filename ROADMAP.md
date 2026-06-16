# Roadmap

This roadmap tracks nbox from skeleton to safe writes. It maps the implementation phases in [DESIGN.md](DESIGN.md) onto release milestones. Items are intentionally read-only first; write support is deliberately deferred.

Legend: ☐ planned · ◐ in progress · ☑ done

## Design pillars

- **Agent-first.** nbox is built so that an LLM/agent is a first-class consumer, not an afterthought. The clean command core (one function per operation, structured output) feeds three surfaces from the same code: the CLI, the TUI, and a **first-class MCP server** (`nbox mcp serve`). The `--json`/`--envelope`/`--fields`/`--raw` controls and `AGENTS.md` are step one; the MCP server is the headline (see v0.2 / v0.3).
- **Read-only first, writes gated.** Every read surface ships before any write. Writes are `PATCH`-based, diff-previewed, and confirmable — and exposed over MCP only behind an explicit opt-in.
- **Correctness over breadth.** Permissive wire models, a typed error layer, and CI against a real NetBox (not just mocks) before adding surface area.

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

## v0.1.1 — Close the gap (correctness before breadth)

A review found v0.1 advertises commands and TUI behaviour that aren't implemented yet. Before any v0.2 work, either ship these or stop documenting them — and pull the cheap, high-leverage read-only wins forward.

- ☐ **Implement `nbox open`** (web URL via `util::format::api_to_web_url` + `open`) — it's a stated v0.1 MVP feature but currently `bail!`s "not yet implemented".
- ☐ **Implement `nbox interface <device> <iface>`** — the device-detail story; currently stubbed.
- ☐ **TUI device-detail tabs** (`i` interfaces · `p` IPs · `c` cables · `v` VLANs) — documented in README/DESIGN, not built. Add an `interfaces`/IP section to `DeviceView`.
- ☐ **Read-only IPAM allocation:** `nbox next-ip <prefix>` / `nbox next-prefix <prefix>` via `GET …/available-ips/` + `/available-prefixes/`. The *read* half of the most-requested NetBox workflow fits the read-only contract; the *claim/allocate* half ships with writes (v0.2).
- ☐ **Typed errors (`src/error.rs`):** map 401→auth, 403→perms, 404→not-found, ambiguous match→disambiguation list (today `device_by_ref` silently takes the first `name__ic` hit). Give not-found a **distinct exit code** (AGENTS.md/DESIGN claim one; `main.rs` exits `1` for everything).
- ☐ **CI against a real NetBox:** `netbox-community/netbox-docker` (pin a 4.x ≥ 4.2 tag) in GitHub Actions, seeded with a deterministic fixture + legacy v1 token, running the built binary (`assert_cmd`) against the live API. Catches serializer drift (polymorphic `scope`, `assigned_object`, `available-ips` shape) that wiremock cannot.
- ☐ Read-only `nbox raw GET <path>` escape hatch (power users + a stopgap for unmodeled types).
- ☐ `config_version` field + forward-compat handling (before v0.2 mutates the config schema).
- ☐ `clap_mangen` man pages alongside the existing completions.
- ☐ Reconcile DESIGN.md with reality: mark absent modules/sections (`error.rs`, `graphql.rs`, `schema.rs`, `docs/` tree) as aspirational so contributors aren't misled.

## v0.2 — Nested views, IPAM power, first writes

- ☐ **First-class MCP server (read-only): `nbox mcp serve`** (stdio first, HTTP after) exposing the command core as MCP tools — `search`, `device`, `ip`, `prefix`, `vlan`, `site`, `status`, `next-ip`/`next-prefix`. This is the headline of the agent-first pillar: same code as the CLI, structured results, no shelling out. Internal agents are the primary early consumer.
- ☐ **Robustness on large instances:** honor HTTP 429 `Retry-After`, bound search/`list_all` concurrency, cap unbounded paging. Required before agents/MCP hammer a production API.
- ☐ **IPAM allocation (write half):** `claim`/`allocate` the next IP/prefix (POST to `available-ips`/`available-prefixes`). The read-only `next-ip`/`next-prefix` ships in v0.1.1.
- ☐ **Cable / interface trace:** `/api/dcim/interfaces/{id}/trace/` — "where is this port cabled to?"; surface on the interface/device detail view
- ☐ **Hierarchical prefix tree in the TUI:** expand/collapse child prefixes with inline utilization (netbox#21396/#21255)
- ☐ **Pick ONE device-detail path** (don't build both): REST fan-out (device + interfaces + IPs) **or** a read-only GraphQL query (device + interfaces + IPs + rack + site in one round-trip). Decide before implementing.
- ☐ Multi-pane TUI (nav | results | detail) per DESIGN mockup, vs current screen-switching
- ☐ IP ranges (`/api/ipam/ip-ranges/` + `available-ips`)
- ☐ **Safe writes (initial):** `PATCH` engine, minimal diff, before/after preview, confirmation modal; agent-safe `--read-only` profile
  - ☐ **Design gate:** write field-coercion + diff rules (choice fields `{value,label}`→bare string; brief relations by slug/id/name; non-TTY/`--json`/MCP confirmation UX) before coding the engine
  - ☐ `nbox device <name> set status <value>`
  - ☐ `nbox interface <device> <iface> set description "..."`
- ☐ `changelog_message` support on writes

---

## v0.3 — Broader models, writes, discovery

- ☐ **Write-capable MCP tools** behind an explicit opt-in (`--allow-writes` / a write-enabled profile): expose the `PATCH`/allocate operations as MCP tools that return the diff for the agent to confirm. Read-only `mcp serve` stays the default.
- ☐ `nbox ip <addr> reserve --description "..."`
- ☐ `nbox tag add <type> <name> <tag>`; tag browsing (`nbox tags`, `--tag <name>` filter)
- ☐ Write workflows surfaced in the TUI edit mode (`e` / `d` / confirm)
- ☐ **`--vrf` resolution:** accept `id | rd (65000:100) | name`, in that precedence; ambiguous name → error listing matches. (Deferred from v0.1 search filters; also fixes the first-match-wins limit in `ip_candidates`.)
- ☐ Circuits (`nbox circuit <id>`, included in search)
- ☐ Aggregates (`/api/ipam/aggregates/`) and ASNs (`/api/ipam/asns/`)
- ☐ Journal entries on detail views (`/api/extras/journal-entries/`)
- ☐ Services (`/api/ipam/services/`) — "what's listening on this device?"
- ☐ Write verbs for `nbox raw <POST|PATCH|DELETE> <path>` (read-only `raw GET` ships in v0.1.1)
- ☐ OPTIONS / OpenAPI **write-capability** discovery per NetBox version (narrowed — filter safety is already handled structurally by the typed per-endpoint allowlist); optional user-facing `schema` command
- ☐ Batch queries from a file (audits)

---

## Later / under consideration

- ☐ Dashboard / overview screen (counts by status, utilization stats, recent changes)
- ☐ Context preservation in the TUI (scroll position + filters per view)
- ☐ OS keyring token storage
- ☐ Virtualization (VMs) and tenancy detail views
- ☐ VRF-aware IP/prefix navigation (built on the v0.3 `--vrf` resolution)

**Reconsidering / likely cut**
- Local SQLite cache (`cache` feature) — questionable for a tool whose value is *freshness*; the in-memory `nucleo` ranking already covers interactive speed, and it adds a bundled-C dep + invalidation complexity. Keep parked unless a concrete large-instance latency problem appears; the `cache` feature is dead weight today.
- ~~MCP server~~ — **promoted to a first-class v0.2/v0.3 feature** (see Design pillars).
- ~~Plugin / custom-command system~~ — **cut**; it's a stated non-goal (plugin framework).
- ~~TurboBulk~~ — **cut**; proprietary Cloud/Enterprise-only plugin and bulk import/export is a stated non-goal.

## Project infrastructure & quality

Patterns proven in the author's other Rust tools (ttl, xfr) worth porting. Themes + the update-notifier are already ported. Release workflow, `install.sh`, the Homebrew template, completions, MSRV, and the keep-a-changelog CHANGELOG already exist.

High-impact, easy:
- ☐ **`cargo-audit` CI** (`.github/workflows/audit.yml`): on Cargo.toml/lock changes + daily cron. Supply-chain safety, cheap.
- ☐ **Pre-commit hooks** (`.pre-commit-config.yaml`): `fmt` + `clippy -D warnings` on commit, `test` on push; document **prek** (fast Rust runner) with a Python `pre-commit` fallback.
- ☐ **`musl` Linux targets** in the release matrix (static, glibc-free binaries) — ttl/xfr ship these via `cross`; nbox currently builds gnu only.
- ☐ **`Dockerfile.release` + GHCR image** built from the musl binary on a minimal Alpine base (CA certs only) → `docker run ghcr.io/lance0/nbox`.
- ☐ **Ship completions as a release artifact** (generate in `release.yml`), not just the runtime subcommand.
- ☐ **MSRV CI job** that pins and verifies `rust-version` (1.88).
- ☐ **`dependabot.yml`** with grouped Cargo updates (one PR/week) + github-actions.

Polish:
- ☐ `CONTRIBUTING.md` (setup, style, hooks, PR/commit conventions).
- ☐ `docs/` tree — `ARCHITECTURE.md`, `CONFIG.md`, `FEATURES.md` (lean README, deep reference); the README currently links to `AGENTS.md` only.
- ☐ `KNOWN_ISSUES.md` (candid limitations + workarounds; e.g. first-match disambiguation, no VRF scoping yet).
- ☐ Split runtime state (`prefs.toml`: last theme/profile) from user config (`config.toml`), per xfr's `prefs.rs` (CLI > config > prefs precedence). Pairs with the v0.1.1 `config_version` work.
- ☐ `examples/config.toml` with documented profiles; `.github/FUNDING.yml`.

## Explicit non-goals (v0)

Full CRUD for every model · replacing the NetBox web UI · plugin framework · topology diagrams · bulk import/export · custom script runner · approval workflow engine.
