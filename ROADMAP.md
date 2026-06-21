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
  mode (RFC 9728 metadata, audit log, per-caller rate limit), 9 read tools + a `nbox://{kind}/{ref}`
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

- ☑ **TUI search filters** — surface the CLI's `--status` / `--site` / scope / `--vrf` filters in the
  TUI (filter chips + palette + `f` modal) so the TUI is as capable a search as the CLI.
- ☑ **Dashboard / overview home** — a landing screen: counts by status, top-utilized prefixes, recent
  journal/changelog activity.
- ☑ **Hierarchical prefix tree** — expand/collapse children with inline utilization (netbox#21396/#21255).
- ☑ **TUI context preservation** — scroll position + active filters retained per view across navigation.
- ☑ **Profile cycle order** — cycle profiles in config-file order (an order-preserving map) rather than
  alphabetical.
- ☑ **Cross-object navigation** — jump between related objects from a detail without re-searching. The
  object-level back-stack (`detail_nav`, `b`/`Esc` walks the drill path) and header-relation jumps (the
  `R` modal: device→site/rack, ip→parent-prefix, prefix→vlan, …) ship, and every detail's
  *contained-object lists* are now navigable like the VRF view: ☑ Prefix → children + contained IPs
  (`c`/`a` tabs) · ☑ Device → IP addresses + VLANs (`p`/`v` tabs) · ☑ VLAN → prefixes (`p` tab) · ☑
  Site → devices + racks (`d`/`r` tabs) and Rack → devices (`d` tab). Enter opens the highlighted row,
  `b`/`Esc` walks back through the drill path.
- ☐ **Demo recording** — an asciinema/VHS cast for the README.
- ☑ **Deepen the in-app Config modal.** The profile editor now sets the knobs that used to need a
  hand-edited `config.toml`: per-surface API backends (`[profiles.<name>.api]` `vrf`/`route_target` =
  `rest`|`graphql`, cycled with `Ctrl+B`/`Ctrl+R`), `timeout_secs` + `page_size` (numeric fields), and
  `exclude_config_context` (`Ctrl+E`). REST/default values stay out of the file (the `[api]` table is
  dropped when empty). Deliberately NOT surfaced (no-op toggles, like the long-excluded `confirm_writes`):
  the `search` backend (always falls back to REST) and `confirm_writes` (writes deferred).
- ☑ **Settings-section connection parity (hot-apply).** The Config modal's **Settings** section now has a
  **Connection** category exposing the active profile's `page_size`, `timeout_secs`,
  `exclude_config_context`, and the per-surface `[api]` `vrf`/`route_target` backends (`rest`/`graphql`
  cycles), seeded from the live profile. Saving a change persists it to that profile (format-preserving)
  and **reconnects** through the existing switch path so it hot-applies — the client bakes these at
  construction. (The profile editor remains the place to manage *any* profile; Settings is the quick-tweak
  surface for the *active* one.) The api backends were folded in after the initial parity pass.
- ☑ **Release `0.2.0`** — banked the large read surface accumulated since `0.1.1` (MCP HTTP/OAuth, the new
  read commands, MCP resources, the in-app config layer, three hardening rounds).
- ☑ **Release `0.4.0`** — per-surface API backends (breaking), REST-canonical search (GraphQL search
  dropped), route targets + VRFs first-class, the connection-pool timeout fix, live-browse, the
  config/keyring layer, dashboard, prefix tree, and cross-object nav. Shipped to crates.io / Homebrew
  tap / GHCR via the `/ship` skill.
- ☑ **Release `0.5.0`** — route-target relation graph as a GraphQL accelerator surface, and kind-aware
  browse list columns (per-kind secondary column, content-fit width). Shipped to crates.io / Homebrew
  tap / GHCR via the `/ship` skill.
- ☑ **Release `0.6.0`** — cross-object navigation (navigable contained-object lists on prefix/device/
  VLAN/site/rack detail tabs + a detail tab/footer discoverability cue), `nbox_get` accepting the
  `ip_address` search kind, an in-app profile editor for the API/timeout/page-size/exclude knobs, config
  contract tests, `scripts/smoke.sh`, the Dependabot `rand` ignore, and ratatui 0.30.2. First release to
  use the auto-extracted CHANGELOG release notes. Shipped to crates.io / Homebrew tap / GHCR.

---

## Foundation before scale

These are the highest-leverage engineering items before the repo grows much more. Bias toward small,
reviewable PRs that lock contracts and reduce future change cost.

- ☑ **Golden JSON contracts, first slice** — file-backed contracts for `status`, `search`, and
  `device_detail`, rendered through the shared JSON renderer.
- ☑ **Shared test support layer** — `tests/support/` builders/helpers for representative fixtures,
  rendered JSON assertions, binary execution, and wiremock NetBox pages.
- ☑ **Binary error contracts, first slice** — process-level tests for exit codes `1`/`2`/`3`/`4`/`5`,
  clean stdout on errors, and actionable stderr.
- ☑ **Broaden output goldens** _(PR #16, #17)_ — file-backed JSON contracts for `ip`, `prefix`, `vlan`,
  `interface`, `site`, and `journal` (a journal-bearing response), via the shared `assert_golden` harness.
  The next best guardrail for agents and scripts.
- ☐ **CSV/output-mode contracts** — pin CSV shape for list/search output, `--cols` ordering, empty
  arrays, and the intentional “single objects are not CSV” usage error.
- ☐ **MCP response contracts** — stable JSON shapes for `nbox_status`, `nbox_search`, `nbox_get`,
  resource reads, and MCP error mapping (`invalid_params` vs internal errors). Keep these against
  direct server calls, not brittle protocol snapshots.
- ☐ **Fixture migration pass** — move repeated inline NetBox JSON in `search_tests`, `query_tests`,
  `scope_tests`, MCP tests, and custom-field tests onto `tests/support` builders as those files are
  next touched.
- ☑ **Compatibility matrix as tests + docs** _(PR #21)_ — `tests/compat_tests.rs` (9 tests) pins the 4.2
  scope model, 4.3 REST-only search, 4.5 client-side utilization + v2 tokens, and version-floor gating;
  `docs/COMPATIBILITY.md` documents the matrix (cross-checked against the official release notes — citing the
  documented changes, marking the prefix-`utilization` absence and `/api/status` auth as observed-not-noted).
- ☐ **CLI contract harness** — a thin reusable harness for command-level tests that records
  `(args, stdout, stderr, exit_code)` expectations while preserving the stdout-data-only invariant.
- ☑ **Release smoke checklist automation** — `scripts/smoke.sh` runs the release-critical gate in one
  shot (`fmt`, both clippies, both test modes, `cargo audit`, build smoke, man-page + completion
  generation) before tags move. Referenced from `CONTRIBUTING.md`. (Cross-compiled musl/darwin/windows
  builds stay the release workflow's matrix, not the local smoke.)
- ☐ **Observability contracts** — pin `nbox status`, MCP status, and selected debug/audit fields so
  users and agents can tell backend, version, capability, and failure mode without scraping prose.
- ◐ **Config migration/compat tests** — token-source precedence (`select_env_token`), the onboarding
  predicate (`needs_onboarding_for`), redaction (`config show`/`Debug`), and format-preserving edits
  (comments + unrelated keys survive; `save_setting_fields` atomic) are locked in `config.rs` tests.
  ☐ Remaining: explicit old/future `config_version` shape fixtures (forward-compat warn is covered; a
  versioned-migration matrix is not yet needed).
- ☐ **Dependency and feature matrix** — CI or scripted local checks for default, `--no-default-features`,
  `http`, `keyring`, `keyring-secret-service`, and release-musl-relevant feature combinations.
- ☐ **Performance baseline, narrow** — bench or measured smoke for search fan-out and JSON rendering
  on representative fixture sizes. Do not add a cache unless measurements justify it.

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

- ☐ **Cable-path visualizer (idea — exploring).** A TUI rendering of an interface's cable trace: the
  A-side ↔ Z-side path (with any intermediate panels / junctions) drawn inline on the interface/cable
  detail, from the trace data nbox already fetches. Scoped to a **single connection's path**, NOT full
  network topology maps (those stay an explicit non-goal). Raised 2026-06-20.
- ☑ **Full rack integration** — racks are now a first-class **searchable** `ObjectKind`: they appear in
  the global search fan-out (REST + GraphQL), `/` + `nbox search`, MCP `nbox_search`, and a `rack`
  palette lookup, honoring the site/region/site-group/location scope (like devices, via `*_id`). They
  were already openable + a cross-nav target in the TUI (`nbox rack <ref>`, device→rack). ☑ **Rack
  elevation** — the rack detail has an `e` (elevation) tab: a framed, U-by-U front view from NetBox's
  `/elevation/` endpoint (devices fill their U span, name on the top row), with rack-assigned-but-
  unpositioned devices listed below. ☐ Optional: rear face + a CLI `--elevation`.
- ☑ **Multi-pane TUI** (nav │ results │ detail) per the DESIGN mockup. The home screen gained a left
  Navigation rail: browse-by-kind sections (Devices/Prefixes/IPs/VLANs/Sites/Racks) with domain-colored
  bullets and live per-kind counts, plus Recent; `Enter` lists a kind into Results (search stays on `/`),
  `Tab` cycles the three panes. Built on the list/preview split.
- ☑ **3-pane polish (follow-ups).** Right-aligned Nav counts (display-width measured), a Recent
  count, the Route Targets section (Nav label abbreviated to "RTs"), **remember the last-browsed kind**
  (persisted to `[ui].last_browsed` on exit; restored on launch — cursor lands on it and its list
  preloads, focus stays on Nav), **live-browse on Nav `j`/`k`** (moving the rail cursor auto-browses the
  highlighted kind into the results pane — debounced until the cursor settles so a fast scroll doesn't
  flash intermediate lists; focus stays on Nav, `Enter` still commits + jumps into the list), and a
  Nav-focused footer hint (`j/k browse · Enter results`).
- ☑ **Kind-aware browse list columns.** A homogeneous browse (the Nav rail opening one kind) now drops
  the redundant per-row KIND tag — the pane title already names the kind — and labels the secondary column
  with the attribute that kind carries in `browse.rs` (`STATUS` for prefixes/IPs, `VID` for VLANs,
  `RD/TENANT` for VRFs, `TENANT` for route targets, `SITE` for devices/racks, `SLUG` for sites — via
  `ObjectKind::subtitle_header`), tinting the header with the kind's domain color and sizing the column to
  its content. Header and values agree (the labels match what `browse.rs` actually puts in the subtitle).
  Site-less kinds no longer read as a ragged, empty SITE column; mixed search results + Recent keep the
  generic `KIND/DISPLAY/SITE` layout. (A richer multi-column layout — e.g. device name/site/role/status —
  would need `SearchResult` enriched with those fields; deferred.)
- ☑ **VRF-pivoted navigation (a dedicated VRF view).** VRF is now a first-class `ObjectKind`:
  searchable (REST + GraphQL), browsable from the Nav rail with a live count, `nbox vrf <name|rd|id>`,
  palette `vrf`, `open`/`journal` resolvers, and MCP `nbox_get`/`nbox://vrf/<ref>`. The TUI detail is a
  routing context — a fixed header card (RD/tenant/RT/enforce) over the VRF's prefix tree (navigable
  summary slot) with navigable `addresses` and a `targets` tab. Built on the new navigable-detail-row
  mechanism (a `DetailRow { text, target }`; `Enter` opens, `b`/`Esc` returns).
- ☑ **Per-surface API backends.** Replaced the coarse `backend` profile key with `[profiles.<name>.api]`
  (`search`/`vrf` = `rest`|`graphql`). REST is canonical; a GraphQL surface is an opt-in accelerator
  resolved against the live schema probe (`EffectiveBackend`, REST-fallback with reason). Surface-aware
  capabilities + a per-surface `api` block in `nbox status`/MCP. VRF GraphQL fetches its prefix/address
  bundle in one query; REST and GraphQL produce a byte-identical `VrfDetail`.
- ☑ **Search stays REST — GraphQL search dropped (decided 2026-06-19).** Investigated collapsing the
  per-kind GraphQL search fan-out into one bundled POST. NetBox 4.3+ GraphQL has **no `q` full-text
  filter** (filtering moved to per-field Strawberry lookups), so it can't reproduce canonical NetBox
  search — the old fan-out silently returned unfiltered first-pages on 4.3+. Decision: `nbox search` is
  REST-canonical; GraphQL never backs it (a `search = "graphql"` preference transparently falls back).
  Removed the GraphQL search path entirely. The single-POST idea survives as a *different* future
  surface (see typeahead below).
- ☐ **GraphQL `browse`/typeahead surface (distant).** A single aliased `*_list` POST filtering each kind
  by its name/description via `StrFilterLookup` `icontains` — a fast name-substring lookup for TUI
  typeahead/incremental browse. Explicitly **not** `search`: different, non-canonical semantics (won't
  match serial/tag/custom-field hits the way REST `q` does). Ship it as its own opt-in `[api]` surface,
  honestly labeled as name/description filtering, where the UI can say so. Long-horizon.
- ☐ **GraphQL accelerator candidates (tracked).** GraphQL fits a surface when it can *bundle* a
  bounded set of related objects behind *exact* filters with a clean REST fallback — and is wrong for
  anything that means canonical full-text search. Prioritize as the TUI detail/browse contracts settle;
  each must keep REST canonical and stay backend-neutral in output (one view shape, like `VrfDetail`).
  - ☑ **VRF detail** — shipped. Header + `prefixes` + `addresses` in one `vrf_id`-scoped POST.
  - ✗ **Dashboard / home overview — SKIPPED (poor GraphQL fit, 2026-06-21).** The dashboard's cost is
    *counts* (total + 6 status buckets = 7 of its 9 calls), which REST does cheaply (`limit=1` → read
    `page.count`). Probed live 4.5: GraphQL has **no count aggregation and no `total_count`** —
    `device_list` returns a bare `[DeviceType]`, so a count means fetching the full id list. Bundling the
    dashboard into one POST would fetch every device id ×7 (and the status filter is an enum, and journal
    `kind` is value-only) — a regression at any real scale. GraphQL accelerates *bundling related objects*,
    not *counting*. See [[nbox-graphql-shapes]].
  - ☐ **Browse / list panes** — Nav rail opening `VRFs`/`Sites`/`Prefixes`/`Devices` with sort/limit/
    basic filters, fetching exactly the columns the TUI renders. Frame as browse/filter, not search
    (overlaps the typeahead surface above).
  - ✗ **Device detail bundle — SKIPPED (not byte-identical, 2026-06-21).** Probed the live 4.5 schema:
    NetBox GraphQL returns enum *values* as plain strings with no label/display variant (`InterfaceType`
    exposes `type -> String` = `"10gbase-x-sfpp"`), but the REST device detail renders the interface
    **type label** (`SFP+ (10GE)`, via `IfaceRow.type_ = c.label`). A byte-identical bundle would need a
    client-side ~100-entry interface-type `value→label` map kept in sync across NetBox versions — exactly
    the brittle maintenance the accelerator bar avoids. (`status`/service `protocol` use `.value`, fine;
    role/site/vlan/cable use `.label()`=display, which GraphQL can supply — interface `type` is the lone
    blocker, and it's load-bearing on the most-used tab.) VRF/RT worked because their data is flat strings
    with no value/label duality; the device detail is enum-label-heavy, so it doesn't fit. See
    [[nbox-graphql-shapes]].
  - ✗ **Prefix detail bundle — SKIPPED (not byte-identical, 2026-06-21).** Probed live 4.5.10:
    `PrefixFilter` has **no `within`/`within_include`/any descendant lookup** — its only network filters are
    `contains` (the *opposite*, supernet direction: `contains:"10.10.5.0/24"` → `["10.10.0.0/16"]`) and
    exact `prefix`. The children tab is built with REST `?within=<cidr>`; GraphQL can't express that without
    pulling the whole prefix table and filtering client-side (a scale regression, already rejected for the
    dashboard). The IP half *would* reproduce byte-identically (`IPAddressFilter.parent` works, and the
    `assigned_object` union `... on InterfaceType { name device { name } }` reshapes to REST's
    `display`/`device.display` so the existing `assigned_label` is byte-identical) — but accelerating only
    IPs yields **zero round-trip reduction**. Deeper: children/IP filters both need the prefix's cidr+vrf_id,
    which only the header fetch provides, so even a GraphQL bundle is header(REST)+bundle = 2 round-trips —
    identical to the pure-REST concurrency fix below. See [[nbox-graphql-shapes]].
  - ☐ **Make prefix-detail children + IPs concurrent (pure-REST, byte-identical micro-win).** The prefix
    detail currently fetches `prefix_children` then `prefix_ips` **sequentially** (two awaits in
    `domain/detail.rs`'s `ObjectKind::Prefix` arm); `build_vrf_detail` already runs its two child fetches
    with `tokio::try_join!`. Mirror that for prefix → header(REST) + concurrent children+IPs = 2 round-trips,
    the same latency the (infeasible) GraphQL bundle targeted, with no new backend and trivially identical
    output. This is the actual deliverable that the prefix accelerator was standing in for.
  - ☐ **VLAN / tenant detail views** (once the TUI detail contract settles) — VLAN (VLAN + prefixes +
    group/scope), tenant (tenant + devices/prefixes/IPs summary). Read-only GraphQL alternatives to the REST
    fan-outs; only pursue where the fan-out is a real latency cost, the relations sit behind *exact* filters
    (NetBox GraphQL has no hierarchy/`within` lookups — see the prefix skip), and the view has no
    value-only-enum label like device's interface type. Don't maintain both surfaces for a view indefinitely.
  - ☑ **Route-target / routing-context views** _(PR #22)_ — route targets are a first-class object
    (lookup, search, open, journal, MCP); the detail's importing/exporting VRF relation graph is now a
    GraphQL accelerator surface (`[profiles.<name>.api] route_target = "graphql"`): one
    `route_target_list` query replaces the two REST `vrfs` list calls, identity stays REST, output is
    byte-identical, with REST fallback. **Track status (2026-06-21): exhausted.** VRF + route-target are the
    only two accelerators; device, dashboard, and prefix were each probed live and skipped (see above). No
    further accelerator surfaces are planned — the prefix latency win is a pure-REST concurrency fix.
- ☑ **GraphQL backend cleanup.** Typed `GqlVrf{Prefix,Address}` + `VrfBundleResponse` structs replace
  the `from_value(json!{})` row reshape (`Default` on the `Prefix`/`IpAddress` wire models lets the
  conversion set only the VRF-relevant fields). All GraphQL — capabilities probe + VRF bundle + helpers
  + tests — now lives in `netbox/graphql.rs`; `search.rs` is REST-only (2657 → ~1.2k lines).
- ☐ GraphQL capability probing v2 if schema churn demands it: dynamic `*Filter` discovery and/or a
  short TTL cache keyed by instance/profile to avoid re-probing when users bounce between profiles
  pointing at the same NetBox.
- ☑ **Local cache (2026-06-19).** A small, bounded **in-memory** view-model cache (keyed by
  profile+kind+ref) so a burst of identical reads doesn't re-hit NetBox. Single short TTL (default 30s,
  a *de-dupe* window, not a freshness window — nothing is served past TTL); `r`/auto-refresh/profile-
  switch always bust; a dim "cached Ns ago" footer chip surfaces age. Shipped for TUI **detail**
  navigation; configurable via `config.toml [cache]`. An on-disk SQLite version was built then
  deliberately walked back (staleness + a large on-disk cache are the wrong trade for an infra tool).
  ☑ Settings-modal toggle for `enabled`/`ttl_secs` (hot-applied). **The CLI intentionally does NOT
  cache** — it's one-shot (resolve→print→exit), so an in-memory cache has nothing to reuse, and "always
  fresh from source" is the right default for the scripting/automation surface; no `--no-cache` /
  `nbox cache clear` (nothing persistent to bypass or clear). The cache is a long-lived-process feature.
  ☑ **MCP cache** — `nbox serve` reads (`nbox_get`) go through the cache (chatty agents re-reading the
  same object graph de-dupe within the TTL), with an `nbox_cache_clear` tool so agents can force fresh
  reads after out-of-band changes. ☑ **Preview-pane caching** — the results preview shares the detail
  cache key, so scrolling back over seen rows is instant and a preview warms the cache for opening that
  object (and vice versa). Cache is now complete across surfaces (TUI detail + preview, settings toggle,
  MCP reads + clear; CLI intentionally none). Optional follow-up: ☐ MCP `cached_at`/age annotation
  (short TTL + the clear tool already cover most of it).
- ☑ **Single binary.** One canonical full-featured binary per platform: the default feature set
  carries every cross-platform user feature (`http`, native `keyring`, `clipboard`, `updates`), no
  feature-variant artifacts. `--no-default-features` stays a dev-only lean build;
  `keyring-secret-service` (D-Bus) stays off so the musl static build links clean. Release builds derive
  the feature set from `default` (no redundant `--features` flags). MSRV dropped 1.95 → 1.88 (the 1.95
  floor was a leftover of the removed on-disk cache; stale `cache`-feature docs cleaned up).
- ☐ Batch queries from a file (audits).
- ☐ Configurable client concurrency for very large instances — `search` is a bounded fan-out and
  `list_all` is `max`-capped today; expose tuning only if a real instance needs it.
- ☐ TurboBulk export — capability-detect `/api/plugins/turbobulk/`, read/export-only (JSONL, no
  arrow/parquet dep), behind a feature flag, clean fallback when absent. Fast full-table export/audit
  on large instances where paginated REST is too slow.
- ☐ Split `prefs.toml` (runtime state) from `config.toml` (user config), per xfr. Pairs with
  `config_version`.

**Reconsidering / likely cut**

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
  limit, 9 read tools, `nbox://{kind}/{ref}` resources.
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
- ☑ MSRV CI job (pins `rust-version` 1.88).
- ☑ Real-NetBox integration workflow (`netbox-integration.yml`).
- ☑ **Auto-populate the GitHub Release body from the CHANGELOG.** The `release` job now
  extracts the curated `## [X.Y.Z]` section from `CHANGELOG.md` (awk between the tag's
  heading and the next `## [`) into `body_path`, with `generate_release_notes: true`
  appending GitHub's "What's Changed" PR list + full-changelog link below it — so the
  published notes match the changelog automatically, no by-hand patching. Falls back to
  auto-notes (with a `::warning::`) if the section is missing — warn-and-fallback is the
  deliberate choice; a hard tag-fails-without-an-entry check was considered and declined
  (2026-06-20).
- ☑ `clippy::pedantic` enforced whole-project (incl. test crates) via a `Cargo.toml [lints]` table.
- ☑ Golden output contracts + shared integration-test support (`tests/golden/`, `tests/support/`).
- ☑ Binary-level error contracts for stable exit codes and stdout cleanliness.
- ☑ `dependabot.yml`, `CONTRIBUTING.md`, the `docs/` tree, `KNOWN_ISSUES.md`, `examples/config.toml`,
  `.github/FUNDING.yml`.

### Code nits to revisit (verified 2026-06-19, post live-browse)

- ☑ **Profile switch leaves the live-browse flags unreset** _(done, PR #18)_ (`tui/state.rs` `clear_for_profile_switch`).
  It clears `browse_kind`/`preview_dirty` but not `browse_dirty`/`nav_tick_anchor`, so whether the new
  instance auto-browses the hovered Nav section depends on whether a `PreviewTick` fired mid-switch (the
  `switch_in_flight` guard consumes the flag). Correct-by-accident today; make it deliberate — either reset
  `browse_dirty = false` + `nav_tick_anchor = nav_selected` for a clean empty pane, or set
  `browse_dirty = true` to always reload the hovered kind on the new instance.
- ☑ **Exit persists theme + last_browsed in two separate writes** _(done, PR #18)_ (`tui/app.rs` `run_on`). Each is a full
  read-modify-write of `config.toml`; if both changed it writes twice, and a failure between them
  half-persists. Batch into one `config::save_ui_fields(&[Theme, LastBrowsed])` — the atomic batch helper
  already exists and is tested.
- ☑ **`connect_timeout` is hardcoded 10s, independent of the configurable overall `timeout`** _(done, PR #18)_
  (`netbox/client.rs:53`; overall = `timeout_secs.unwrap_or(15)`). With `timeout_secs < 10` the overall
  timeout fires first (reqwest takes the min) — harmless but confusing. Clamp:
  `connect_timeout = min(10s, timeout.saturating_sub(1s))`.
- ☑ **(test) `live_browse_on_recent_clears_the_results` checks state, not the recents render.** _(done, PR #18)_ It asserts
  `browse_kind == None` + empty view but seeds no recents, so it doesn't prove the fallback paints. Seed a
  recent and assert `home_target()` falls back to it.
- ☑ **MCP search → get kind chaining.** `nbox_search` returns `kind = "ip_address"` while `nbox_get`
  canonically uses `ip` (the only divergence — every other kind already matches). Rather than change the
  pinned search output, `GetKind` now accepts `ip_address` as a non-breaking alias for `ip` (serde alias on
  the tool arg + `from_str` for `nbox://ip_address/…`), so an agent can chain search → get without
  translating. Documented in `AGENTS.md` / `docs/MCP.md`.
- ☑ **De-dup the 429-retry loop** (`netbox/client.rs` `send()` vs `graphql()`). The copy-pasted
  `if 429 && attempt < MAX_RETRIES { sleep; retry }` wrapper is now a shared `retry_on_rate_limit(&res,
  attempt, what)` helper (owns `MAX_RETRIES`, honors `Retry-After`/`backoff`, tags the warn line by `what`);
  both loops just `if retry_on_rate_limit(..).await { attempt += 1; continue }`. Sidestepped the
  GET-params-vs-POST-body fiddliness by passing the already-sent `&Response` instead of a request closure.
  Locked end-to-end by a wiremock test (429 + `Retry-After: 0` → retried → 200).
- Considered, not worth doing: `nav_section_index_for_slug` linear scan over 9 slugs (a `match` would be
  exhaustive, but the list is tiny); `status_in_banner` elevating only Warning/Error (deliberate — long
  Info/Success messages are transient and stay in the footer slot); the error-body `truncate()` allocating
  via `chars().take().collect()` (required for UTF-8 char-boundary safety on a rare error path — a zero-copy
  slice could panic mid-codepoint); `list_all` buffering up to `max` rows in memory (bounded by the caller's
  cap — fine for a one-shot read CLI; streaming would only matter for an unbounded export, which we don't do).

### Performance candidates (evaluated 2026-06-21, agent + code verification)

A batch of proposed perf wins, each verified against the code. Net: one quick win, one medium, one probe; the rest skip. The search path is **network-dominated** — CPU micro-opts there are noise.

- ☐ **Concurrent scope+VRF resolution (quick win).** `search.rs` resolves `--scope` then `--vrf` as two
  **independent sequential awaits** (`resolve_scope` ~`:329`, `resolve_vrf` ~`:337`) before the 17-way
  fan-out. `tokio::try_join!` them — saves 1-4 RTTs on *filtered* searches, zero risk. (No filter ⇒ both
  return `Ok(None)` with zero network calls, so the win only applies when a scope/vrf filter is set.) NOTE:
  the broader "fire the fan-out optimistically alongside resolution" idea is **unsound** — the fan-out's
  filters need the resolved ids (`site_id`/`vrf_id`/scope content-type), so it can't start blind; a
  cancellation token doesn't help when the input is the missing value.
- ☐ **TUI render dirty-flag (idle-CPU win, medium).** The event loop `terminal.draw`s on every event, and
  the 180ms `PreviewTick` always arrives → a full widget rebuild ≥5.5 Hz even when idle (500-row `Vec<Row>`
  clones + `format!`). ratatui diffs the *buffer* (I/O minimal) but not the Rust-side rebuild. A render-dirty
  flag would skip the redraw when nothing changed — a battery/SSH win, not latency. CARE: the tick also
  advances the spinner, status-TTL expiry, and the browse/preview debounce flush, so the flag must key on
  *state mutation* (and still mark dirty on spinner ticks, status changes, async results) — not on "no
  keypress," or it freezes the spinner / stalls TTL.
- ☐ **HTTP/2 multiplexing — probe spike first (low priority).** reqwest's `http2` feature is **off** (the
  `h2` crate in the lockfile is axum's MCP server, not the outbound client), so the client can't negotiate
  h2 today. h2 *would* sidestep the gunicorn half-close (multiplexing is per-connection) — but only if the
  operator runs **nginx with h2 on the listener**; gunicorn **sync** workers are HTTP/1.1-only. Spike: probe
  whether real instances negotiate h2; only then weigh enabling the feature with a capability fallback.
- ✗ **Connection pooling `pool_max_idle_per_host(1)` — SKIP (dangerous).** Directly reverts the documented
  fix at `client.rs:60-69` and reintroduces the half-closed-socket stall (sync gunicorn FINs right after each
  response; a reused idle socket stalls to the 15s timeout). No client-side idle timeout reliably dodges a
  server that closes per-response. The premise is also overstated — the 17 fan-out connections open
  *concurrently* (~1 RTT of parallel handshake), not 2-3s serial. See [[netbox-gunicorn-keepalive]].
- ✗ **Skipped micro-opts (verified negligible):** radix/patricia trie for the prefix tree (the O(n·d)
  coverage scan runs **once at load**, off the render thread, n≤1000); cache-eviction O(n) oldest-scan (only
  on a full-1024 insert, microseconds, and a new dep vs the hand-rolled cache); `nav_counts` array-vs-HashMap
  (8 entries); `score_match` `to_lowercase()` allocations (on the network-return path, dwarfed by the
  round-trips — and not a clean swap: `starts_with`/`contains` have no ASCII-case-insensitive stdlib form,
  and names can be Unicode; if ever touched, lowercase the query *once* outside the per-result loop).

### Dependency maintenance

- ⏸ **`rand` held at `0.8`.** `rsa 0.9.10`'s `RsaPrivateKey::new` (test-only keygen, `mcp/http.rs`) requires
  a `rand_core` 0.6 RNG; `rand` 0.9/0.10 moved to `rand_core` 0.9, so the bump doesn't compile. Pinned on
  purpose (`Cargo.toml` comment). **Unblock when `rsa` ships on `rand_core` 0.9**, then take the bump and
  switch `thread_rng()` → `rng()`. Dependabot PR #15 (group bump incl. `rand` 0.10) is parked on this.
- ☑ **Ungroup Dependabot cargo updates.** `dependabot.yml` now `ignore`s `rand`'s minor/major bumps (0.8.x
  patches still flow), so the held `rand` (⊥ `rsa` 0.9's `rand_core` 0.6) no longer blocks every other safe
  crate in the grouped PR — no more manual hand-bumps like the `ratatui` 0.30.2 one. Unpin the ignore when
  `rsa` ships on `rand_core` 0.9 (then take rand 0.9+ and switch `thread_rng()` → `rng()`).
- ☑ **GitHub Actions on Node 24.** Bumped `actions/cache@v5`, `actions/upload-artifact@v7`, and the
  `docker/*` actions (Dependabot #4–8, 2026-06-20) to clear the Node-20 deprecation warnings in `release.yml`.

## Explicit non-goals

Full CRUD for every model · replacing the NetBox web UI · a plugin framework · topology diagrams · a
bulk import/export engine (TurboBulk export aside) · a custom script runner · an approval-workflow engine.
