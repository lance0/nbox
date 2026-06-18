# Roadmap

nbox is a **read-only** NetBox CLI, TUI, and MCP server. The near-term goal is the **best
possible read experience** — fast, correct, and pleasant both in the terminal and to agents.
Write support is a deliberate *later* track (see [Writes — deferred](#writes--deferred-later-track));
it lands only once the read tool is proven in practice.

Legend: ☐ planned · ◐ in progress · ☑ done

## Principles

- **Read-only is the product (for now).** Reads ship and get polished before any write surface.
  When writes come they'll be `PATCH`-based, minimal-diff, before/after-previewed, confirmable,
  and opt-in — never the default.
- **Agent-first.** CLI, TUI, and `nbox serve` (MCP) run off one command core; `--json`/`--envelope`/
  `--fields`/`--raw` + `AGENTS.md` make every read scriptable, and the same views back the MCP tools.
- **Correctness over breadth.** Typed errors, real-NetBox integration CI, and ambiguity surfaced
  (never silently guessed) before more surface area.

---

## Shipped — the read-only product

The read surface is broad and stable today (full history in `CHANGELOG.md`):

- **CLI lookups — 19 object types:** `device`, `interface`, `ip`, `prefix`, `vlan`, `site`, `rack`,
  `circuit`, `provider`, `aggregate`, `asn`, `ip-range`, `tenant`, `contact`, `vm`, `cluster`, plus
  `search`, `journal`, `tags`, `status`, `open`, `raw GET`. NetBox 4.2+ polymorphic scope + VRF
  correctness; ambiguous refs exit `5` with the candidate list.
- **Search:** parallel multi-endpoint fan-out with `--status` / `--site` / `--region` /
  `--site-group` / `--location` / `--tenant` / `--role` / `--tag` / `--vrf` filters (per-endpoint
  allowlist, resolved to ids); fail-closed with `--partial` for best-effort.
- **IPAM read:** `next-ip` / `next-prefix` (available, VRF-scoped), prefix utilization, cable/interface
  trace, VRF-scoped child prefixes + contained IPs.
- **Output:** `-o plain|json|csv`, `--raw`, `--envelope`, `--fields`, `--cols`; stable exit codes.
- **MCP server (`nbox serve`):** stdio **and** HTTP (Streamable HTTP), OAuth 2.1 OIDC resource-server
  mode (RFC 9728 metadata, audit log, per-caller rate limit), 8 read tools + a `nbox://{kind}/{ref}`
  resource template (DESIGN §24; read-only Pattern 3).
- **TUI:** list/preview split with focus, scrolling + position cues, 11 themes, command palette,
  fuzzy filter, recents, auto-refresh, device tabs, open-in-browser/copy, profile switcher
  (`P`/`Ctrl+P`), and an in-app **Config modal** (`S`) — profile editor (add/edit/select/delete),
  settings, and **first-run onboarding**.
- **Secrets:** OS keyring token storage with env fallback (`token_env` → `NBOX_TOKEN` → keyring);
  the token is never written to `config.toml` or logs.
- **Distribution & quality:** release matrix (musl + darwin + windows), Homebrew tap, GHCR image,
  shell completions + the full man-page set, crates.io; real-NetBox integration CI; whole-project
  `clippy::pedantic` gate. The detailed v0.1 / v0.1.1 records are kept below.

---

## Now — best-in-class read-only UX (current focus)

Polish the read experience. No writes here.

- ☐ **TUI search filters** — surface the CLI's `--status` / `--site` / scope / `--vrf` filters in the
  TUI (filter chips / palette) so the TUI is as capable a search as the CLI.
- ☐ **Dashboard / overview home** — a landing screen: counts by status, top-utilized prefixes, recent
  journal/changelog activity.
- ☐ **Hierarchical prefix tree** — expand/collapse children with inline utilization (netbox#21396/#21255).
- ☐ **TUI context preservation** — scroll position + active filters retained per view across navigation.
- ☐ **Profile cycle order** — cycle profiles in config-file order (an order-preserving map) rather than
  alphabetical.
- ☐ **Demo recording** — an asciinema/VHS cast for the README.
- ☑ **Release `0.2.0`** — banked the large read surface accumulated since `0.1.1` (MCP HTTP/OAuth, the new
  read commands, MCP resources, the in-app config layer, three hardening rounds).

---

## Writes — deferred (later track)

Writes are intentionally **not** near-term. They land after the read tool is proven in practice, behind
explicit opt-in (a write profile / `--allow-writes`, with `confirm_writes` already groundwork),
`PATCH`-based with a before/after diff + confirmation, and read-only staying the default everywhere.
Consolidated future scope:

- ☐ **Safe `PATCH` engine** — minimal diff, before/after preview, confirmation modal; agent-safe
  read-only default. Settle write rules first (choice fields `{value,label}`→string, brief relations
  by slug/id/name, confirmation semantics in non-TTY / `--json` / MCP).
- ☐ `nbox device <name> set status <value>` · `nbox interface <device> <iface> set description "…"` ·
  `nbox ip <addr> reserve --description "…"` · `nbox tag add <type> <name> <tag>`.
- ☐ **IPAM allocate** — claim the next IP/prefix, plus IP-range `available-ips` (POST to
  `available-ips` / `available-prefixes`); the read half (`next-ip` / `next-prefix`, range lookup)
  already ships.
- ☐ `changelog_message` support on writes.
- ☐ **Write-capable MCP tools** — opt-in, return the diff for the agent to confirm; read-only stays the
  default — plus the **per-user credential vault (Pattern 2)** for real per-user NetBox RBAC over MCP.
- ☐ TUI edit mode (`e` / `d` / confirm).
- ☐ `nbox raw POST|PATCH|DELETE`; OPTIONS write-capability discovery (optional `schema` command; would
  also enable value-level filter validation beyond today's typed allowlist, netbox#6489).
- ☐ **Agent write ergonomics** — a `--dry-run` convention and per-command skill files, landing with
  writes (`AGENTS.md` is the read-side foundation today).

---

## Later / under consideration

- ☐ Multi-pane TUI refinement (nav | results | detail) per the DESIGN mockup, building on the current
  list/preview split.
- ☐ VRF-pivoted navigation in the TUI (a dedicated VRF view) — the `--vrf` filter, VRF-scoped prefix
  sections, and exact VRF-by-RD lookup already ship; this is the navigation layer on top.
- ☐ GraphQL detail views after the TUI detail experience settles — start with device detail as a
  read-only GraphQL query alternative to the REST fan-out; only pursue if the fan-out becomes a
  latency problem, and don't build both surfaces indefinitely.
- ☐ GraphQL backend cleanup once PR #11 has review miles: table-driven search descriptors for the
  repeated search branches, shared kind→web-path mapping, and less boilerplate around row IDs.
- ☐ GraphQL capability probing v2 if schema churn demands it: dynamic `*Filter` discovery and/or a
  short TTL cache keyed by instance/profile to avoid re-probing when users bounce between profiles
  pointing at the same NetBox.
- ☐ Batch queries from a file (audits).
- ☐ Configurable client concurrency for very large instances — `search` is a bounded fan-out and
  `list_all` is `max`-capped today; expose tuning only if a real instance needs it.
- ☐ TurboBulk export — capability-detect `/api/plugins/turbobulk/`, read/export-only (JSONL, no
  arrow/parquet dep), behind a feature flag, clean fallback when absent. Fast full-table export/audit
  on large instances where paginated REST is too slow.
- ☐ Split `prefs.toml` (runtime state) from `config.toml` (user config), per xfr. Pairs with
  `config_version`.

**Reconsidering / likely cut**

- Local SQLite cache (`cache` feature) — the value is freshness, and `nucleo` already covers
  interactive speed; it adds a bundled-C dep + invalidation complexity. Parked unless a real
  large-instance latency problem appears.
- Plugin / custom-command system — cut; a non-goal.

---

## Shipped history — v0.1 / v0.1.1

<details kept inline for the record; everything below is ☑ done.>

### v0.1 — Read-only foundation

- ☑ **Phase 1 (skeleton):** `clap` CLI + global flags; config loader + `config init/path/show`;
  profile commands; auth schemes (`auto`/`bearer`/`token`); `reqwest` client (TLS/timeout); token
  redaction in logs; paginated `Page<T>` + `list`/`list_all`; `/api/status/` probe + 4.2 floor;
  JSON output; CI green from commit 1.
- ☑ **Phase 2 (core models):** `BriefObject`/`Choice<T>`/`Tag`/custom fields; device/interface/ip/
  prefix/vlan/site/rack (+ vrf/tenant); endpoint mapping + per-endpoint queries; normalized
  multi-endpoint search; detail resolution (incl. IP → parent prefix via `ipnet`); plain + JSON.
- ☑ **Phase 3 (TUI v0):** panic-safe init/restore; mpsc event loop; search + results; detail pane;
  nav history; help modal; command palette; `nucleo` fuzzy ranking; open-in-browser; copy.
- ☑ **Phase 4 (polish & release):** 11 themes (cycle + persist); update notifier; friendly errors;
  shell completions; recents; the release pipeline / `install.sh` / Homebrew template / crates.io;
  `nbox status`; prefix utilization; custom fields in detail; structured + scope + `--vrf` search
  filters; CSV output + `--cols`; auto-refresh; `--envelope`/`--fields`/`--raw`; `AGENTS.md`.

### v0.1.1 — Close the gap

- ☑ `nbox open`, `nbox interface`, TUI device tabs (`i`/`p`/`c`/`v`/`s`).
- ☑ Read-only `next-ip` / `next-prefix` (VRF-scoped; `--length`). Allocate lands with writes.
- ☑ Typed errors + stable exit codes (3 auth, 4 not-found, 5 ambiguous).
- ☑ Real-NetBox integration CI (netbox-docker 4.2.x, seeded fixture).
- ☑ Read-only `raw GET`; `config_version` + forward-compat; `clap_mangen` man pages
  (`nbox man` top-level, `nbox man <dir>` full set).

### v0.2.0 — shipped since v0.1.1

- ☑ **MCP server** (`nbox serve`) — stdio + HTTP transport, OIDC resource-server auth, audit + rate
  limit, 8 read tools, `nbox://{kind}/{ref}` resources.
- ☑ **Read coverage** — circuits, providers, aggregates, ASNs, IP ranges, tenants, contacts, VMs,
  clusters; journal command + inline `--journal`; services on device detail; cable/interface trace.
- ☑ **Scope/VRF** — `search --vrf`, scope filters (`--region`/`--site-group`/`--location`), exact
  VRF-by-RD, VRF-scoped prefix child/IP sections.
- ☑ **TUI** — list/preview split + focus, scrolling + position cues, profile switcher, the in-app
  Config modal (profile editor + settings + first-run onboarding).
- ☑ **Secrets** — OS keyring token storage with env fallback.
- ☑ **Hardening** — two review-driven rounds (OIDC/HTTP, scope resolution, installer, man-page
  quality, profile-switch races, allowed-host port validation, etc.).

---

## Infrastructure & quality

- ☑ `cargo-audit` CI (the `audit` job gating every release).
- ☑ Pre-commit hooks (fmt/clippy on commit, test on push).
- ☑ musl Linux targets in the release matrix (static x86_64/aarch64; gnu aarch64 kept).
- ☑ `Dockerfile.release` + multi-arch (amd64/arm64) GHCR publish.
- ☑ Completions + the full man-page set shipped as a release artifact.
- ☑ MSRV CI job (pins `rust-version` 1.95).
- ☑ Real-NetBox integration workflow (`netbox-integration.yml`).
- ☑ `clippy::pedantic` enforced whole-project (incl. test crates) via a `Cargo.toml [lints]` table.
- ☑ `dependabot.yml`, `CONTRIBUTING.md`, the `docs/` tree, `KNOWN_ISSUES.md`, `examples/config.toml`,
  `.github/FUNDING.yml`.

## Explicit non-goals

Full CRUD for every model · replacing the NetBox web UI · a plugin framework · topology diagrams · a
bulk import/export engine (TurboBulk export aside) · a custom script runner · an approval-workflow engine.
