# Roadmap

nbox is a **read-first** NetBox CLI, TUI, and MCP server. The near-term goal is the **best
possible read experience** — fast, correct, and pleasant both in the terminal and to agents.
write engine, with seven commands landed — `interface <device> <iface> set
description` and `device <name> set status <value>` (`PATCH`), `ip reserve
<prefix>` / `prefix reserve <cidr>` / `ip-range reserve <start|id>` (three
`allocate` `POST`s to `available-ips` / `available-prefixes`), and `tag add` /
`tag remove <type> <name> <tag>` (a `PATCH` to the `tags` array on any object
kind). Reads stay the default everywhere and the write surface widens only as
the read tool proves out in practice (see
[Writes](#writes--deferred-later-track)).

Legend: ☐ planned · ◐ in progress · ☑ done

## Principles

- **Read-first; writes are narrow and opt-in.** Reads are the bulk of the product and get polished
  first. The first writes have landed as a gated `PATCH`-based foundation (ADR-0001): minimal-diff,
  before/after-previewed, confirmable, behind `--allow-writes` + confirmation — never the default.
- **Agent-first.** CLI, TUI, and `nbox serve` (MCP) run off one command core; `--json`/`--envelope`/
  `--fields`/`--raw` + `AGENTS.md` make every read scriptable, and the same views back the MCP tools.
- **Correctness over breadth.** Typed errors, real-NetBox integration CI, and ambiguity surfaced
  (never silently guessed) before more surface area.

---

## Shipped — the read-only product

The read surface is broad and stable today (full history in `CHANGELOG.md`):

- **CLI lookups — broad NetBox object coverage:** `device`, `interface`, `ip`, `prefix`, `vlan`,
  `site`, `rack`, `rack-group`, `circuit`, `virtual-circuit`, `provider`, `aggregate`, `asn`,
  `ip-range`, `tenant`, `contact`, `vm`, `vm-type`, `cluster`, `vrf`, `route-target`, `mac`,
  plus `search`, `journal`, `tags`, `tagged`, `status`, `open`, `raw GET`. NetBox 4.2+ polymorphic scope + VRF
  correctness; ambiguous refs exit `5` with the candidate list.
- **Search:** parallel multi-endpoint fan-out with `--status` / `--site` / `--region` /
  `--site-group` / `--location` / `--tenant` / `--role` / `--tag` / `--owner` / `--owner-group` /
  `--vrf` filters (per-endpoint allowlist, resolved to ids); fail-closed with `--partial` for best-effort.
- **IPAM read:** `next-ip` / `next-prefix` (available, VRF-scoped), prefix utilization, cable/interface
  trace, VRF-scoped child prefixes + contained IPs.
- **Output:** `-o plain|json|csv`, `--raw`, `--envelope`, `--fields`, `--cols`; stable exit codes.
- **MCP server (`nbox serve`):** stdio **and** HTTP (Streamable HTTP), OAuth 2.1 OIDC resource-server
  mode (RFC 9728 metadata, audit log, per-caller rate limit), 11 read tools + a `nbox://{kind}/{ref}`
  resource template (DESIGN §24; read-only Pattern 3).
- **TUI:** list/preview split with focus, scrolling + position cues, 12 themes, command palette,
  fuzzy filter, recents, auto-refresh, device tabs, open-in-browser/copy, profile switcher
  (`P`/`Ctrl+P`), and an in-app **Config modal** (`S`) — profile editor (add/edit/select/delete),
  settings, and **first-run onboarding**.
- **Secrets:** the API token lives in `config.toml` (`token = "..."`, `0600` on Unix, redacted in
  display) or an env var — precedence `token_env` → `NBOX_TOKEN` → config token → none, each source
  scheme-prefix/whitespace-normalized. No OS keyring.
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
- ☑ **Selection window on every related-object detail tab** _(0.9.0)_. Device interfaces (`i`) and cables
  (`c`) tabs are navigable lists (`j`/`k` + `Enter`) that open the interface's detail; interfaces are now a
  first-class `ObjectKind` navigation target. The render fix landed under the hood: `render_detail` only drew
  the selection cursor for header-bearing details (VRF/route-target), so every other detail rendered its rows
  as plain text — the tab bar now pins in a fixed band for any tabbed detail and one shared builder renders
  rows everywhere, so the device IP/VLAN, prefix children/addresses, and site/rack device tabs are navigable
  too. Services (`s`) stay text (no detail to open).
- ☑ **Server-side browse filter (Nav rail).** From the Nav rail, `/` on a name-bearing kind
  (devices/racks/sites/VLANs/VRFs/route-targets) filters that list **server-side** by name (`name__ic`)
  instead of opening global search — explicit (Enter to apply, not live typeahead), so it doesn't hammer
  NetBox while typing. The pane title shows the active filter + count (`Devices · name contains "edge" · 52`),
  a `<cap>+` count signals truncation; `Esc` clears (Normal) / cancels the edit (BrowseFilter), `Ctrl+X`/empty-
  Enter clear while editing. Prefix and IP-address now filter by network containment
  (the follow-up item below); aggregate keeps `/` as global search (it keys on a
  CIDR column but isn't a Nav-rail browse kind).
  - ☑ **CIDR-containment filter for prefix/IP browse.** The follow-up for the
    CIDR/inet-keyed kinds: `/` on a prefix browse filters by `within_include`
    (the prefix + everything inside it), on an IP-address browse by `parent`
    (addresses inside the prefix) — so `/` narrows a prefix/IP list by network
    instead of falling back to global search. The value is a CIDR, validated locally
    on Enter (a typo is an instant error, not a NetBox 400); the pane title reads
    `within "10.0.0.0/24"`. Every Nav-rail kind is now filterable; the router's
    `None` → search arm stays as a defensive fallback for a future non-filterable
    browse kind.
- ☑ **Prefix contained IP cap for full `/24`s.** Prefix detail's contained-IP tab
  now fetches up to 512 rows in CLI, MCP, and TUI detail, covering a full IPv4
  `/24` (254 hosts) while child prefixes and other detail tabs stay at
  `DETAIL_SECTION_CAP` (200). Browse intentionally remains capped at
  `BROWSE_CAP` (currently 500) and expects server-side filtering, not scrolling
  through thousands of rows. Do **not** resurrect generic load-more-on-scroll for
  every browse; only add targeted higher caps/load-more on detail tabs when a
  real operator workflow proves the need. The old `offset += page_size` row skip
  is already fixed in 0.12.0.
- ☐ **Capped detail-section truncation cues.** Prefix contained IPs now have a
  boundary test proving the 512-row cap, but larger prefixes can still be
  silently clipped at that detail-section budget. Add a shared view/model cue for
  capped sections (`<cap>+` in the TUI/plain views, and an additive JSON field if
  needed) once we want to make truncation explicit across detail tabs rather than
  only increasing one cap.
- ☑ **Hierarchical scope filters.** `search --region`/`--site-group`/`--location`
  now uses NetBox's native tree-aware scoped id filters (`region_id`,
  `site_group_id`, `location_id`) on prefixes and clusters, so the selected node
  and its descendants are included server-side. `--site` stays exact via the
  polymorphic `scope_type=dcim.site` + `scope_id=<id>` match.
- _Tracked vs by-design (`KNOWN_ISSUES.md` cross-reference, so the two docs stop drifting): the
  browse/sub-resource caps are covered by load-more above; the `offset += page_size` skip is **fixed**
  (0.12.0, `list_all` now follows `next`); hierarchical scope is **fixed** by the item above; read-only
  by **Writes — deferred** and CSV shape by **CSV/output-mode contracts**; device-by-name-vs-display is documented
  (a `device_by_ref` suffix-strip fallback is a candidate fix), not yet scheduled. The remaining two —
  parent-prefix enrichment as a best-effort longest match, and name lookups resolving exact-then-contains
  — are **by design** (surfacing ambiguity over guessing), acknowledged here, not tracked for a fix._
- ☑ **Demo recording** — a VHS cast for the README (`docs/demo.tape`, rendered to `docs/demo.gif`); see the [Demo](README.md#demo) section.
- ☑ **TUI browse kind-switch cache.** Browse-by-kind results are cached for the
  current TUI session by `(kind, filter)`, so returning to a previously-loaded
  Nav kind repaints instantly and restores the row selection while the normal
  NetBox browse refresh still runs in the background. Profile switches drop the
  cache to avoid cross-instance data bleed.
- ☑ **Headless/SSH clipboard (OSC 52).** On Linux/Unix X11+Wayland sessions, `y`-copy uses `arboard`;
  over SSH with no display forwarding it waits out the X11 connection timeout, then fails with a
  cryptic error (the copy runs on a spawned task, so the UI doesn't freeze, but nothing reaches a
  clipboard). Fix: on non-macOS Unix with no `$DISPLAY`/`$WAYLAND_DISPLAY`, skip `arboard` and emit an
  OSC 52 escape (`ESC ]52;c;<base64>BEL`) so the *local* terminal writes the *local* clipboard over the
  SSH stream. macOS and Windows still try `arboard`'s native desktop backends first; any `arboard` error
  falls back to OSC 52 rather than surfacing the X11 message. Document the caveats (tmux needs
  `set -g set-clipboard on`; some terminals disable OSC 52) and keep the status honest ("copied via
  terminal").
- ☑ **Interface journal + `nbox_get interface`.** Interfaces are now a
  first-class `GetKind::Interface` in both `nbox_get` (MCP + resource) and the
  journal resolver, so `nbox journal interface <device>/<name>` and `nbox_get`
  with `kind=interface` work. Interfaces have no single-string ref (addressed
  by device + name, or numeric id), so the resolver takes the compound
  `<device>/<name>` form — the name is taken verbatim after the device, since
  interface names may contain slashes (`xe-0/0/1`, `Ethernet1/49`). Raised in
  PR #64 review.
- ☐ **Filtered cable browse (only filtered).** Cables aren't a browsable kind today (no `ObjectKind::Cable`,
  no cable detail view); they surface via a device's Cables tab and the interface cable-path. A *flat* "browse
  all cables" is a trap at scale (millions of rows), but a **filtered** view — cables by site / rack / device /
  status — could be useful. Would need `ObjectKind::Cable` + a cable detail view + scoped filters; never a
  flat list. Raised in PR #64 review.
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
- ☑ **Release `0.7.0`** — first-run UX (oriented empty home screen + a recoverable connect banner / "connected
  to NetBox vX" cue instead of a hard exit), in-Settings connection editing (the Connection category:
  page_size/timeout_secs/exclude_config_context + the `[api]` vrf/route_target backends, hot-applied via
  reconnect), concurrent prefix-detail and scope+VRF fetches, deepened CSV/MCP contract tests, the
  persist_profile/429-retry refactors, and dependency maintenance (sha2 0.11, rust-toolchain pinned to MSRV).
  Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.7.1`** — onboarding/token-handoff fixes: cancel the wizard's leaked terminal-event reader
  (the post-onboarding "first keypress eaten / had to close out" freeze); block a pasted token when no
  persistent OS keyring exists (default Linux/musl) instead of silently losing it; make profile token saves
  transactional (keyring change prepared before the TOML write, rolled back on failure); and keep a profile
  rename working when the keyring is unavailable (metadata renames, best-effort token-migration warning).
  Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.7.2`** — keyring storage is now **opt-in**: a token pasted in onboarding or the Settings
  profile builder saves to `config.toml` by default (redacted from display; file `0600` on Unix) and just
  connects — no keychain prompt. Reverses 0.7.1's block-pasted-token friction. Opt into the OS keyring with
  `Ctrl+K` / `nbox config token set` (`token_store = "keyring"`, moves the token to the keychain, clears the
  TOML token). Precedence: `token_env` → `NBOX_TOKEN` → config token → opt-in keyring → none. Deferred minors:
  `config init` 0644, the 0600 write-then-chmod race, Windows perms, a cosmetic keyring-migrate message.
  Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.8.0`** — **remove the OS keyring entirely.** It was net-negative surface: a frictional prompt
  on some platforms, an "unavailable" dead-end on default Linux/musl, and a whole transactional machinery to
  keep two stores in sync. The token now lives only in `config.toml` (`token = "..."`, `0600`, redacted) or an
  env var; precedence collapses to `token_env` → `NBOX_TOKEN` → config token → none, with each source
  scheme-prefix/whitespace-normalized before it competes (so `NBOX_TOKEN="Bearer "` can't mask a valid config
  token). Drops the `keyring`/`keyring-secret-service` Cargo features, `nbox config token set`/`clear`, the
  TUI `Ctrl+K` toggle, and the `token_store` key (now ignored). `config token status` stays. Also: `config init`
  + every config write keep the token file `0600` across the whole write; TUI `Ctrl+T` test-connect shares the
  normalized resolution; README/CONFIG call out `token` vs `token_env`. Migration: re-enter any keyring-stored
  token as a config `token` or `token_env`. Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.8.1`** — fix a site-browse timeout on large instances. NetBox's full site list serializer runs
  per-site aggregate-count subqueries (device/prefix/rack/vlan/circuit); on a real 149-site instance the full
  list took >120s (timeout) while the nav count and every other browse kind returned in <1s. The site browse now
  requests NetBox's `brief` representation (name + slug, the only columns it shows) — ~400× faster, no column
  loss; the detail view still fetches the full object. Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.9.0`** — **interfaces become first-class in the TUI.** A new `interface` `ObjectKind`
  (navigation/detail target, not in global search); device interfaces/cables tabs are navigable and open the
  interface detail. A **cable-path visualizer** draws the trace as a vertical A↔Z diagram (device emphasized,
  patch panels as pass-through stops, unterminated sides explicit) — a TUI "cable path" tab and an inline
  section in `nbox interface`; cable views name the **far device**. Fixes folded in: header-less detail tabs
  now render their selection cursor (also un-breaking the device IP/VLAN, prefix children, site/rack tabs);
  Nav-rail counts abbreviated + the rail widened so large instances don't clip; `nbox raw GET` accepts a path
  with or without `/api/` and rejects absolute URLs. Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.10.0`** — **circuit terminations + A↔Z path.** `nbox circuit <cid>` resolves the
  circuit's A/Z terminations and renders the path (walking through patch panels to the far device, since
  NetBox exposes no `/trace/` for circuit terminations) as a vertical A↔Z diagram; the TUI circuit detail
  gains a `p` path tab + navigable links to the provider/sites/devices along the path; `-o json` / MCP
  `nbox_get` / `nbox://circuit/{ref}` carry a structured `terminations` array (each hop's `path` includes a
  `device` ref + the rendered `diagram` lines). Also `nbox profile remove <name>` (refuses the active/only
  profile), a first-run onboarding redesign (URL-led three-field screen, profile name derived from the URL
  host), and a `--no-tui` setup-hint fix (the URL is a positional, not `--url`). Shipped to crates.io /
  Homebrew tap / GHCR.
- ☑ **Release `0.11.0`** — **server-side browse filter.** From the Nav rail, `/` on a name-bearing kind
  (devices/racks/sites/VLANs/VRFs/route-targets, circuits by `cid`) filters that list server-side by name
  (`name__ic`/`cid__ic`) instead of opening global search — explicit (Enter to apply), with a filter+count
  pane title; prefix/IP keep `/` as global search (CIDR/inet columns have no `__ic` lookup, so a name filter
  there would silently match the whole table). The Nav-rail browse cap was briefly raised 500 → 1000
  (still one round trip — NetBox's per-request ceiling), then reverted to 500 once the render cost of
  rebuilding idle lists was measured; filtering is the scale path. Shipped to crates.io / Homebrew tap / GHCR.
- ☑ **Release `0.12.0`** — **NetBox 4.2–4.6 kind & field coverage + agent hardening.** New first-class
  kinds: `virtual-circuit` (4.2), `mac` reverse-lookup, `rack-group` + `vm-type` (4.6); the cross-cutting
  `owner` field + `--owner`/`--owner-group` search filters (4.5); NAT inside/outside on `nbox ip` and a
  cross-kind reverse-tag lookup (`nbox tagged`); interface as a first-class journal/`nbox_get` kind; a
  credential preflight in `nbox status` (4.5). Robustness: `list_all` now follows the server's `next`
  cursor (fixes a row-skip when a server caps `MAX_PAGE_SIZE` below the request), concurrent prefix detail,
  and a search 404-swallow on version-gated endpoints. Agent surface: every `nbox_get` kind view pinned by
  a response-contract test, a schema-drift canary against a pinned OpenAPI snapshot, and `nbox serve
  --print-config` install recipes. Shipped to crates.io / Homebrew tap / GHCR.

---

## Keeping current with NetBox (4.6 → 4.7)

NetBox has moved to 4.6 (tick-tock cadence; 4.7 "tock" — may break — is the next watch).
A 2026-06 feature scan surfaced read-only, non-goal-respecting surface nbox doesn't yet
cover. All of these stay within the read-only product and the explicit non-goals.

- ☑ **MAC addresses as a first-class kind** _(NetBox 4.2)_. `nbox mac <addr>`
  reverse-resolves a MAC to the interface(s)/device(s) that carry it — a top
  operator/agent query nbox couldn't answer. Any common MAC form is normalized
  first (a non-MAC is a usage error, no round-trip); MACs aren't globally unique,
  so several carrying interfaces surface as ambiguous (exit 5) with the candidate
  list. Polymorphic assignment (physical interface *or* VM interface) renders as
  `"<parent> <interface>"`. On the CLI, MCP (`nbox_get` kind `mac` /
  `nbox://mac/<addr>`), and `nbox open mac/<addr>`. Lookup-only (exact
  `mac_address=`) — not browsable/searchable, since MACs aren't substring-meaningful.
  Highest value, shipped.
- ☑ **`virtual-circuit` (+ terminations, 4.2).** A full first-class kind: `nbox
  virtual-circuit <cid|id>`, `nbox_get kind=virtual_circuit`, `nbox journal
  virtual-circuit <cid>`, `nbox open virtual-circuit/<cid>`, `nbox://virtual_circuit/<cid>`
  resource, and a search fan-out. Virtual circuits are multi-point overlays on
  device interfaces — no A/Z sides, no cables — so the view is flat attributes + a
  flat termination list (each termination's `device`/`interface` refs, for
  navigation), not a cable-path diagram. Verified against the live 4.6.2
  OpenAPI schema for the model shape.
- ☑ **New object kinds from 4.6.** The 4.6 additions `virtual-machine-type`,
  `rack-group`, `cable-bundle` — small, formulaic lookups that keep kind coverage
  current. (Each: model + `nbox <kind>` + detail view; `cable-bundle` pairs with
  the cable-path visualizer.) `rack-group` and `vm-type` shipped; `cable-bundle`
  remains, deferred to its cable-path-visualizer PR.
- ☑ **`owner` field + `--owner` filter** _(4.5)_. NetBox added a native `owner` (users/groups)
  on most objects — structured ownership that beats tag-scraping for agents. Surface it on
  detail views and as a search filter.
- ☑ **Reverse tag lookup** _(4.3, `/api/extras/tagged-objects/`)_. `nbox tagged <tag>` answers
  "what objects carry tag X" across kinds in one call (tag resolves by id/name/slug; each row
  carries a friendly `kind`/`object_type`). Distinct from `search --tag`, which needs a `q` and
  filters per-endpoint.
- ☑ **NAT inside/outside enrichment** _(4.6)_. `nbox ip` surfaces `nat_inside` (on a NAT
  outside IP) and `nat_outside` (on the inside IP); both omitted when absent (byte-identical for
  non-NAT IPs). The device IP tab picks it up for free.
- ☐ **Cable-profile / bundle-aware cable-path visualizer** _(4.5 cable profiles, 4.6 CableBundle)_.
  Breakout/lane cables otherwise trace inaccurately — keep the 0.9.0 visualizer correct on new NetBox.
- ☐ **4.7 compatibility watch.** Re-verify the matrix against 4.7 when it lands (~Aug 2026).
  GraphQL depth-cap defense for 4.6's `GRAPHQL_MAX_QUERY_DEPTH` is shipped: effective GraphQL
  bundle failures warn and retry REST. Docs: `docs/COMPATIBILITY.md`.
- ☐ **4.6 pagination primitives (infra).** Optional, version-gated: evaluate REST/GraphQL cursor
  pagination (`start`/`limit`) for future large fan-outs, with clean fallback to the REST `next`
  link or today's bounded GraphQL first page. Do **not** plan HTTP cache revalidation until NetBox exposes a
  read-validator path; nbox's current cache deliberately lives at the assembled
  view-model layer because NetBox's `ETag` is not a useful `304` read-cache signal.
- ☑ **Credential preflight via `/api/authentication-check/` (4.5).** A dedicated token-validity probe
  (cleaner than inferring auth from `/api/status/`) surfaced in `nbox status` and MCP `nbox_status` as
  a `token` field (`valid`/`invalid`/`unverified`, the authenticated user on `valid`). Best-effort:
  never errors, overlaps the capability probe, and the `nbox status` exit-code contract is unchanged
  (a rejected token during the status fetch still exits 3).
- ☑ **Schema-drift canary (CI).** Pins a compact NetBox OpenAPI snapshot
  (`tests/schema/netbox-4.6.2.json` — bare GET filter params per search endpoint)
  and a `schema_canary` test (`src/netbox/search.rs`) that validates the search
  fan-out's declared `search_supported()` filter set against it: a filter nbox
  sends that the pinned release doesn't accept (e.g. the `tenant`-on-rack-groups
  silent-over-broad bug the canary caught on first run) fails the build with the
  exact endpoint + filter. Refresh the snapshot against a new release
  (`scripts/gen_schema_snapshot.py` from `/api/schema/`) and the canary flags
  the drift before it reaches users. Lightweight — nbox stays hand-curated; this
  is just an early-warning signal.

## Agent / MCP wedge

A 2026-06 competitive scan confirmed the wedge is unoccupied: nobody else ships **CLI + TUI +
read-only MCP in one (Rust) binary**. The benchmark competitor is the official
`netboxlabs/netbox-mcp-server` (read-only, Python, 3 generic tools). These items widen the lead;
all read-only. (Market positioning itself stays out of the repo — see private notes.)

- ☐ **Lean into "the agent-native NetBox binary."** One static musl binary, zero runtime, drops into
  any agent sandbox — vs the all-Python MCP field. Worth a measured cold-start/latency comparison.
- ☑ **MCP prompts catalog.** Curated read-only prompt templates for common
  investigations advertised via `prompts/list` + `prompts/get`: `ip_utilization_audit`,
  `cable_path_trace`, `find_stale_prefixes`, `object_change_review`. Each returns a
  user-role message with a structured investigation plan naming the exact nbox tools
  to call (incl. `nbox_history`), tailored to the supplied arguments. Zero live
  dependency — a prompt is a plan, not data; the agent runs the plan against the
  tools. `enable_prompts()` capability advertised; the catalog is static (no NetBox
  round-trip). Wired the same way as the manual `list_resources` path (the
  `#[tool_handler]` macro only emits tool methods).
- ☐ **Token-budget discipline as a headline.** Lean default view models + `--fields` across CLI/MCP;
  document per-tool token footprints (the official server's headline is ~90% reduction via field filtering).
- ☑ **First-class install recipes.** Copy-paste MCP config for Claude Code / Desktop / Cursor, plus an
  `nbox serve --print-config` helper. (SKILL.md + the README "Add it to Claude" block are the start.)
  `--print-config` now prints the paste-ready `mcpServers` JSON (absolute binary path, echoed
  `--profile`/`--config`, placeholder token) and exits; docs/MCP.md lists the per-host config-file path.
- ☑ **Per-domain agent-skills catalog (write domain).** The first skill files
  landed for the write surface, in the standard agent-skills layout
  (`skills/<domain>/SKILL.md`): `skills/writes/` (the universal lifecycle),
  `skills/ipam-allocate/`, `skills/tag-writes/`, `skills/patch-writes/`.
  Flag-free by design — each points at `nbox <cmd> --help` so it can't silently
  drift; `scripts/lint_skills.sh` + a `skills-lint` CI workflow check the
  frontmatter shape. The root `SKILL.md` indexes the catalog and now mentions
  the write surface. ☐ Read-domain skills (search, IPAM read, device/interface
  context, `serve`) remain — grow the catalog incrementally.
- ☑ **Read-only history/changelog tool + `--diff`.** `nbox history <kind> <ref>`
  shows the system-recorded create/update/delete timeline for an object (who, when,
  and which fields changed) from `/api/core/object-changes/` (NetBox 4.x), distinct
  from `journal` (operator notes). Each row carries `time`/`action`/`user`/
  `object`/`message`/`fields_changed`/`request_id` — the top-level fields whose
  values differ pre vs post. MCP `nbox_history` mirrors it. `--diff` (CLI) /
  `diff=true` (MCP) additionally includes the full `before`/`after` change payloads
  per row — the full JSON for a single change (CLI `--diff` implies `--limit 1`),
  closing the loop on the compact `fields_changed` summary. Answers agent "what
  happened to this prefix?" queries.
- ☐ **Structured read-only exports.** An export mode producing Prometheus targets / firewall
  address-lists / device inventories from live NetBox (the `netbox-lists` niche, as one fast binary).

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
- ☑ **CSV/output-mode contracts** — CSV shape for list/search output, `--cols` ordering, empty
  arrays, and the intentional “single objects are not CSV” usage error are all pinned in
  `tests/csv_contracts_tests.rs` (cols ordering, unknown cols, empty arrays, single-object
  rejection at render/emit/binary-exit-2, comma quoting).
- ☑ **MCP response contracts** — stable JSON shapes for `nbox_status`, `nbox_search`, every
  `nbox_get` kind view, `nbox_get_interface`, `nbox_journal`, `nbox_list_tags`, `nbox_tagged`,
  `nbox_next_ip`/`nbox_next_prefix`, resource reads, and the MCP error mapping
  (`invalid_params` vs internal errors) are all pinned against direct server/tool calls in
  `src/mcp/tests.rs::contracts` (not brittle protocol snapshots). Keep this number-free so new
  kinds cannot stale the roadmap.
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
  `http`, and release-musl-relevant feature combinations.
- ☐ **Performance baseline, narrow** — bench or measured smoke for the known scale paths:
  search fan-out row counts, TUI idle redraw cost, preview fetch count, rack/circuit detail fan-out,
  MCP resource cache reuse, and JSON/CSV output streaming. Keep it small and contract-like: a few
  representative fixture sizes plus "requests made" assertions, not a broad benchmark suite.

---

## Writes — deferred (later track)

The safe-write **foundation** has landed ([ADR-0001](docs/adr/0001-safe-write-foundation.md)): the
shared `MutationPlan`/`MutationReceipt` engine plus the narrow `interface … set description` pilot,
gated behind `--allow-writes` + confirmation, `PATCH`-based with a before/after diff, read staying the
default everywhere. The **broader** write surface stays a deliberate later track — it widens only as
the read tool proves out in practice. Consolidated future scope:

- ☑ **Safe `PATCH` engine** — minimal diff, before/after preview, confirmation; agent-safe
  read-only default. The ADR-0001 foundation landed: a shared `MutationPlan` /
  `MutationReceipt` engine + the `nbox interface <device> <iface> set description
  "…"` pilot (`--allow-writes` + `--confirm`/`--dry-run`, `ETag`/`If-Match` on
- ☑ `nbox interface <device> <iface> set description "…"` — the first write
  command (on the ADR-0001 foundation).
- ☑ `nbox device <name> set status <value>` — the second write command,
  reusing the ADR-0001 foundation. Allowed `status` values are enumerated live
  from NetBox (read-only `OPTIONS`) and the operator's input is normalized to
  the canonical value (a label is accepted case-insensitively when it maps
  unambiguously to one value); unknown/ambiguous status is a usage error before
  any `PATCH`. Same `--dry-run` / `--allow-writes --confirm` / `--message` /
  ETag+`If-Match` / pre-4.6 fallback / local write-audit contracts as the
  interface pilot. The smallest reusable choice-validation mechanism
  (`src/netbox/choices.rs`) backs it, not a generic schema editor.
- ☑ `changelog_message` support on writes — opt-in via `--message`, validated to
  NetBox's 200-character limit before applying; recorded in the object-change
  entry (never logged locally beyond a present-flag + length).
- ☑ `nbox ip reserve <prefix>` — the first **`allocate`** write (a `POST` to
  `…/prefixes/{id}/available-ips/`, not a `PATCH`): reserve the next available IP,
  scoped by `--vrf`, with optional `--description` / `--dns-name`. Server-allocated
  and race-safe, so the plan carries no client precondition; the receipt returns
  the created IP object. Same `--dry-run` / `--allow-writes --confirm` /
  `--message` / local write-audit contracts as the `PATCH` pilots.
- ☑ `nbox prefix reserve <cidr>` — the second **`allocate`** write (a `POST` to
  `…/prefixes/{id}/available-prefixes/`): reserve the next available child
  prefix, optionally with `--length N` to request a specific prefix length and
  `--description`. Server-allocated and race-safe, so the plan carries no
  client precondition; the receipt returns the created prefix object. Same
  gate/confirm/audit lifecycle as the `PATCH` pilots and `ip reserve`.
- ☑ `nbox ip-range reserve <start|id>` — the third **`allocate`** write (a
  `POST` to `…/ip-ranges/{id}/available-ips/`): reserve the next available IP
  address within an IP range, with optional `--description` / `--dns-name`.
  Server-allocated and race-safe, so the plan carries no client precondition;
  the receipt returns the created IP object. Same gate/confirm/audit lifecycle
  as `ip reserve` and `prefix reserve`.
- ☑ **Multi-IP allocation (`--count N`).** `ip reserve` and `ip-range reserve`
  accept `--count N` (default 1) to reserve N IP addresses in one invocation.
  The v1 implementation issues N sequential `POST`s (one IP per request); the
  receipt carries a JSON array of created `IpView`s. `count` is bound into the
  confirmation token. Partial failure (k of N POSTs succeeded) returns the k
  created IPs with `partial: true` and exit 1; the audit logs `outcome=partial`.
  `count=1` plans/receipts are byte-identical to existing single-IP ones.
- ☐ **Atomic multi-IP allocation (list-body POST).** NetBox's `available-ips`
  endpoint accepts a JSON list body (`[{…}, …]`) for all-or-nothing allocation
  in a single round-trip — the server either creates all N or creates zero. The
  current sequential-POST approach (above) permits partial states (handled
  honestly with `partial: true` + exit 1, but the operator still has k orphan
  IPs to clean up) and costs N round-trips. Switching to the list-body POST
  would eliminate the partial-failure path and cut latency to one request; the
  touch points are small (POST body builder + response parser already handles
  single `IpView` vs. JSON array). Revisit if orphan-IP cleanup burden shows up
  in practice.
- ☐ **IPAM allocate (rest)** — choosing a specific address/block. The read half
  (`next-ip` / `next-prefix`, range lookup) and all three allocate writes (`ip
  reserve`, `prefix reserve`, `ip-range reserve`) plus multi-IP `--count N`
  landed above.
- ☑ `nbox tag add <type> <name> <tag>` — a further write command, reusing the
  same foundation. Adds a tag to any taggable object via a minimal `PATCH` that
  replaces the full `tags` array; a no-op if the tag is already present. The
  first **list-valued** writable field and the first write that works on **any
  object kind** (the planner reads the object as a raw value, since every
  NetBox object carries the same `tags` array shape — no per-kind model).
- ☑ `nbox tag remove <type> <name> <tag>` — the inverse of `tag add`, sharing
  one planner/applier (`TagOperation::Add`/`Remove`). A no-op if the tag is
  already absent.
- ☐ **Write-capable MCP tools** — opt-in, return the diff for the agent to confirm; read-only stays the
  default — plus the **per-user credential vault (Pattern 2)** for real per-user NetBox RBAC over MCP.
- ☐ TUI edit mode (`e` / `d` / confirm).
- ☐ `nbox raw POST|PATCH|DELETE`; OPTIONS write-capability discovery (optional `schema` command; would
  also enable value-level filter validation beyond today's typed allowlist, netbox#6489).
- ☑ **Agent write ergonomics** — per-domain skill files for the write
  surface, in the standard agent-skills layout (`skills/<domain>/SKILL.md`):
  `skills/writes/` (the universal dry-run/confirm/audit lifecycle), `skills/
  ipam-allocate/` (`ip`/`prefix`/`ip-range reserve`), `skills/tag-writes/`
  (`tag add`/`remove`), and `skills/patch-writes/` (`interface set
  description` / `device set status`). Flag-free by design — each points at
  `nbox <cmd> --help` so it can't silently drift. A `scripts/lint_skills.sh`
  CI lint checks the frontmatter shape (`name` + `description`).

---

## Later / under consideration

- ☐ **Position-aware circuit panel walk.** `front_for_rear` takes the *first* front-port mapped to the
  circuit's rear-port. Correct for a single-position panel; a multi-position rear (an MPO trunk) maps several
  front-ports at distinct `rear_port_position`s, so first-match could pick the wrong front and show a
  confidently-wrong onward device. Refinement: match the position the circuit's cable lands on against
  `rear_port_position`. Deferred — circuits observed so far land on single-position rears (first-match is
  correct for them), and pinning down how a circuit records its position on a multi-position rear needs a real
  such termination to model. Raised 2026-06-23.
- ☑ **Cable-path visualizer** _(shipped 0.9.0)_. A TUI rendering of an interface's cable trace: the
  A-side ↔ Z-side path (with any intermediate panels / junctions as pass-through stops) drawn from the trace
  data nbox already fetches, as a "cable path" tab on the interface detail and an inline section in `nbox
  interface`. Scoped to a **single connection's path**, NOT full network topology maps (those stay an
  explicit non-goal). Raised 2026-06-20.
- ☑ **Full rack integration** — racks are now a first-class **searchable** `ObjectKind`: they appear in
  the REST-canonical global search fan-out, `/` + `nbox search`, MCP `nbox_search`, and a `rack`
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
  searchable via REST-canonical search, browsable from the Nav rail with a live count, `nbox vrf <name|rd|id>`,
  palette `vrf`, `open`/`journal` resolvers, and MCP `nbox_get`/`nbox://vrf/<ref>`. The TUI detail is a
  routing context — a fixed header card (RD/tenant/RT/enforce) over the VRF's prefix tree (navigable
  summary slot) with navigable `addresses` and a `targets` tab. Built on the new navigable-detail-row
  mechanism (a `DetailRow { text, target }`; `Enter` opens, `b`/`Esc` returns).
- ☑ **Per-surface API backends.** Replaced the coarse `backend` profile key with
  `[profiles.<name>.api]` (`vrf`/`route_target` = `rest`|`graphql`; `search` is accepted
  but REST-canonical). REST is canonical; a GraphQL surface is an opt-in accelerator
  resolved against the live schema probe (`EffectiveBackend`, REST-fallback with reason).
  Surface-aware capabilities + a per-surface `api` block in `nbox status`/MCP. VRF GraphQL
  fetches its prefix/address bundle in one query; route-target GraphQL fetches importing/exporting
  VRFs in one query; REST and GraphQL produce byte-identical detail views.
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
  - ☑ **Make prefix-detail children + IPs concurrent (pure-REST, byte-identical micro-win).** The prefix
    detail's shared CLI/MCP path (`prefix_view_by_ref`) fetched `prefix_children` then `prefix_ips`
    **sequentially**; it now mirrors the TUI `ObjectKind::Prefix` arms with one `tokio::try_join!` →
    header(REST) + concurrent children+IPs = 2 round-trips, with no new backend and trivially identical
    output. (The TUI arms were already concurrent; this finishes the CLI/MCP shared path.)
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
- ☐ GraphQL capability probing v2 if schema churn demands it: dynamic `*Filter` discovery, a
  short TTL cache keyed by instance/profile, and `/graphql/v2` / `GRAPHQL_DEFAULT_VERSION`
  handling if NetBox changes the default GraphQL API version.
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
  ☑ **MCP cache** — `nbox serve` `nbox_get` reads go through the cache (chatty agents re-reading the
  same object graph de-dupe within the TTL), with an `nbox_cache_clear` tool so agents can force fresh
  reads after out-of-band changes. ☑ **Preview-pane caching** — the results preview shares the detail
  cache key, so scrolling back over seen rows is instant and a preview warms the cache for opening that
  object (and vice versa). ☑ MCP resource reads route through the same `nbox_get` cache, so attached
  resources and direct `nbox_get` calls share the within-TTL view. Remaining cache work is performance
  polish, not correctness: ☐ alias ref-loaded detail views to their id key
  after the resolved `DetailView { kind, id, .. }` is known; ☐ consider short-TTL caches for repeated
  MCP search/tags/tagged/get-interface calls; ☐ optionally add MCP `cached_at`/age annotation. CLI
  intentionally remains uncached — one-shot commands should read fresh from source.
- ☑ **Single binary.** One canonical full-featured binary per platform: the default feature set
  carries every cross-platform user feature (`http`, `clipboard`, `updates`), no feature-variant
  artifacts. `--no-default-features` stays a dev-only lean build. Release builds derive the feature set
  from `default` (no redundant `--features` flags). (The OS keyring and its `keyring-secret-service`
  D-Bus backend were removed in 0.8.0 — the token lives in `config.toml` or an env var.) MSRV dropped
  1.95 → 1.88 (the 1.95 floor was a leftover of the removed on-disk cache; stale `cache`-feature docs
  cleaned up).
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
  limit, 11 read tools, `nbox://{kind}/{ref}` resources.
- ☑ **Read coverage** — circuits, providers, aggregates, ASNs, IP ranges, tenants, contacts, VMs,
  clusters; journal command + inline `--journal`; services on device detail; cable/interface trace.
- ☑ **Scope/VRF** — `search --vrf`, hierarchical scope filters
  (`--region`/`--site-group`/`--location`), exact VRF-by-RD, VRF-scoped prefix
  child/IP sections.
- ☑ **TUI** — list/preview split + focus, scrolling + position cues, profile switcher, the in-app
  Config modal (profile editor + settings + first-run onboarding).
- ☑ **Secrets** — OS keyring token storage with env fallback (historical; the keyring surface was
  removed in 0.8.0 after proving too frictional).
- ☑ **Hardening** — two review-driven rounds (OIDC/HTTP, scope resolution, installer, man-page
  quality, profile-switch races, allowed-host port validation, etc.).

---

## Infrastructure & quality

- ☑ **`cargo binstall` support.** `[package.metadata.binstall]` maps to the release archives so
  `cargo binstall nbox` fetches the prebuilt binary (no compile) instead of building from source.
  Metadata-only; takes effect from the release that publishes it (crates.io versions are immutable, so it
  can't be retrofitted onto 0.9.0).
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

### Performance and scale candidates (updated 2026-06-24, agents + web + code)

Principle: optimize the rows and round-trips nbox asks NetBox for before micro-optimizing
CPU. NetBox REST lists are paginated and expose a `next` link; nbox already follows it
where it intentionally pages. NetBox GraphQL offset pagination is not a general scale
escape hatch, and GraphQL still cannot replace canonical REST `q` search.

- ☑ **Concurrent scope+VRF resolution.** `search.rs` resolved `--scope` then `--vrf` as two
  independent sequential awaits before the fan-out; now `tokio::try_join!`ed — saves 1-4 RTTs on
  filtered searches, zero risk, byte-identical results. Do **not** fire the fan-out before resolution:
  the branches need resolved ids (`site_id`/`vrf_id`/scope content-type).
- ☑ **Search per-endpoint row cap (highest backend win).** `SearchRequest.limit` may be 25/50, but each
  branch previously fetched the profile `page_size` before the merged result was truncated. A broad search
  deserialized/ranked ~20 endpoints × 100 rows to return 25. Done: a `list_limited` method caps each branch
  at `min(page_size, max(req.limit, SEARCH_BRANCH_FLOOR))` (floor 25), preserving the global
  merge/sort/dedupe/truncate behavior. A `limit=` regression test pins the cap.
- ☑ **Cheap/cancellable TUI preview (abort handles).** `LoadPreview` uses the same full detail path as `Enter`; a
  second scroll-settle can land while the first fetch is still in flight (NetBox detail fetches take hundreds
  of ms to seconds), so the abandoned task ran to completion and was dropped on arrival. The event loop now
  tracks the in-flight preview `JoinHandle` and aborts it when a new preview starts — freeing the connection
  + CPU instead of letting the superseded fetch finish. Safe with the cache: `get_or_fetch`'s per-key async
  mutex releases on future drop, so a concurrent open of the same object re-acquires and re-fetches (no
  deadlock, no poisoned entry). The stale-response suppression (`PreviewLoaded` dropped if the cursor moved
  on) stays as a backstop. Opening a row still uses the full detail cache ("preview warms open"); the debounce
  + dedup still coalesce a burst of j/k into one load. The remaining options (summary-only preview; longer
  idle delay) are deferred — abort handles is the targeted win for the real scroll scenario.
- ☑ **TUI render dirty-signature (idle CPU/SSH win).** The event loop redraws on the 180ms preview tick even
  when nothing visible changed; `render_home_list` rebuilds/clones the full row set each draw. Use a
  render-signature diff rather than hand-threading a fragile dirty bool. Signature should include visible
  state (`screen`, `mode`, focus, selections, result/detail ids, status+TTL, spinner frame when loading,
  browse/filter state, theme, update banner, terminal size). Failure mode should be over-redraw = status quo,
  never frozen UI.
  - **tick paths that mutate rendered state** (must invalidate the signature): spinner advances *while
    `pending > 0`*; browse/preview debounce flushes *while their dirty flag is set* (the cmd they emit
    redraws on arrival); status-TTL expiry; the async completions (`SearchComplete`/`BrowseComplete`/
    `DetailLoaded`/`PreviewLoaded`/`NavCounts`/`Status`); and `Resize` (terminal size, not `App` state).
  - **test matrix:** idle-no-redraw · spinner-advances-while-loading (then stops on settle) · status-expires
    (redraws while TTL > 0, stops after) · debounce-emits-cmd (no redraw on the tick itself) ·
    async-result-redraws · keypress-redraws · resize-redraws. Extend the existing `TestBackend` +
    `buffer().content` scaffold (`ui.rs:2340`).
- ☐ **Cache render products / viewport-only render.** Once redraw frequency is under control, cache derived
  list data (`sub_w`, owned row cells) when `results/view` changes, and cache active detail-tab lines/counts
  when `(detail id, tab, theme)` changes. For long tabs and prefix-tree views, render only the viewport slice
  instead of every visible row.
- ☐ **Backend fan-out cleanup.** Small, safe latency wins:
  - rack detail: the devices tab (`contained_devices_tab`, a `DETAIL_SECTION_CAP` `Device` list) and the
    elevation tab (`load_rack_elevation`, a *separate* height-bounded fetch — `limit = (u_height*2+4).max(50)`,
    ~88 for a 42U rack, not 1000) are two round-trips today. They return *different shapes* (full `Device`
    objects vs unit-oriented elevation rows), so "share them" is conditional — only worth it when the elevation
    data suffices for the devices tab and that tab isn't rendered from fields the elevation endpoint lacks;
  - ☑ VLAN detail: `vlan_prefixes` and `vlan_group_scope` are independent — `try_join!`;
  - circuit path: memo a panel device's front-port list per detail load so A/Z walks or repeated hops through
    the same patch panel do not page the same 1000 front ports more than once.
- ☐ **Resolver fallback concurrency.** For non-numeric refs, run exact alternatives concurrently where the
  endpoint supports them (slug/RD/name-exact), preserve precedence, and only issue broad `name__ic` fallback if
  all exact probes miss. Keep numeric id as the fast path. Especially helpful for scope filters before search.
- ☐ **Version-gated endpoint availability cache.** On NetBox <4.6, search repeatedly probes 4.6-only
  `rack-groups` / `virtual-machine-types` and swallows 404 as empty. Cache that absence per client after the
  first 404 or derive it once from status/capabilities, while keeping always-present endpoints fail-closed.
- ☑ **MCP resource cache reuse.** `nbox://{kind}/{ref}` resource reads now route through the same cache
  path as `nbox_get`, and `nbox_cache_clear` clears both.
- ☐ **MCP/cache follow-ups.** After a ref load returns a `DetailView`, also populate the id cache key where
  resolver semantics make that safe; consider short-TTL normalized-arg caches for `nbox_search`,
  `nbox_get_interface`, tags/tagged, and journal.
- ☐ **Output streaming fast path.** JSON/CSV output currently materializes `serde_json::Value`/`String` before
  writing. Fast-path no-shaping JSON (`no --fields`, no envelope changes) to `serde_json::to_writer(_pretty)`
  on locked stdout; stream CSV rows instead of building a full string. This matters most for `raw GET`, tagged,
  VRF/detail dumps, and large agent outputs.
- ☐ **MCP/CLI startup and payload budgets.** Load config once for long-lived `serve` startup, skip async runtime
  and update-check setup for pure local commands where practical, and add small size/request-count contracts:
  search per-endpoint `limit`, resource-cache reuse, `tools/list` payload budget, and output streaming behavior.
- ☐ **Counts strategy for large instances.** Nav counts (8 `limit=1` count probes) and dashboard status counts
  (total + status buckets) are concurrent but still force NetBox count work. Keep them because they are useful,
  but consider short TTL/lazy loading and a "counts unavailable / refresh" fallback if real large instances show
  count probes contending with first search/browse.
- ☐ **HTTP/2 / connection reuse probe, not raw HTTP/1 pooling.** Long-lived MCP sessions pay connection churn
  because the client disables idle pooling to avoid stale half-closed HTTP/1 sockets. Do not simply set
  `pool_max_idle_per_host(1)` globally; that reopens the prior gunicorn-style stall. Safer track: enable and
  verify reqwest HTTP/2 where the deployment supports ALPN/multiplexing, or add a profile/server-mode knob with
  conservative defaults and connection-error retry coverage.
- ☐ **Prefix-tree scale pass if the cap rises.** Today the O(n²)-ish child-coverage scan runs once at load under
  a small cap, so it is not urgent. If prefix-tree caps rise or full-tree caching lands, replace it with a stack
  pass and cache visible indices on collapse/expand changes.
- ✗ **Skipped micro-opts (verified negligible for now):** `nav_counts` array-vs-HashMap (8 entries),
  cache-eviction O(n) oldest-scan (bounded, rare), `score_match` lowercase allocations (network-return path and
  Unicode semantics make a "simple" ASCII swap risky). Revisit only with measurements.

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
