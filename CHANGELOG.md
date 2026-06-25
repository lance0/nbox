# Changelog

All notable changes to nbox are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Search per-endpoint row cap.** Each search branch now fetches at most
  `min(page_size, max(req.limit, SEARCH_BRANCH_FLOOR))` rows (floor 25), not the
  full `page_size` (100 by default). The merge truncates to `req.limit` anyway,
  so a `--limit 25` search previously deserialized ~20 endpoints × 100 rows
  (2000 rows) to return 25; it now fetches ~20 × 25 (500). The merge/sort/
  dedupe/truncate behavior is unchanged — each branch just contributes a narrower
  top-K. A floor of 25 keeps each branch wide enough to rank across endpoints
  (nbox's `score_match` is coarser than NetBox's own `q` relevance). A regression
  test pins the per-endpoint `limit=` query param so the cap can't drift back to
  `page_size`.
- **Detail-section cap normalized to 200.** The three detail-view section caps
  (`DEVICE_CAP` 200, `SECTION_CAP` 50, `VRF_SECTION_CAP` 200) collapsed into a
  single `DETAIL_SECTION_CAP = 200` (`src/domain/detail.rs`). `SECTION_CAP = 50`
  was an unprincipled outlier — a VRF's addresses showed 200 but a prefix's child
  IPs (the same kind of data) showed 50, a 4× gap that truncated common `/24`
  prefixes (254 hosts) at 50 rows. Prefix/VLAN child rows now rise to 200 (covering
  the vast majority) and the three-names-one-concept inconsistency ends. The cap
  is a rendering concern (rows in one detail section), so it's named at that
  layer, not the dcim/ipam domain layer. `BROWSE_CAP = 1000` (whole-kind browse,
  a different concept) is unchanged. A full `/24` still truncates at 200; closing
  that fully is a targeted `--all`/fetch-all toggle, not a cap bump.
- **Browse cap reverted 1000 → 500.** A Nav-rail browse now pulls up to 500 rows
  (0.11.0 raised it to 1000; reverted). 500 is still a single round trip (it's
  below NetBox's per-request `MAX_PAGE_SIZE` ceiling of 1000), so the network
  cost is identical — but the rows past a few hundred are essentially never
  scrolled to (at that scale the filter narrows, not the cap), and 1000 doubled
  the TUI's idle render work: the list `Vec<Row>` is rebuilt on every draw
  (~5.5 Hz while the 180ms `PreviewTick` fires; see the ROADMAP `TUI render
  dirty-flag` item) for rows no one reads. The filter is the escape hatch, not
  a bigger cap. The list-count badge reads `500+` when capped. (The dirty-flag
  fix would eliminate idle rebuilds for *all* list sizes — a separate, scheduled
  win; it doesn't change that 500 is the right cap either way, since every
  *change* still rebuilds the full list and 1000 stays 2× the cost at those
  moments.)

## [0.12.0] - 2026-06-24

### Added

- **Schema-drift canary.** A compact NetBox OpenAPI snapshot
  (`tests/schema/netbox-4.6.2.json` — bare GET filter params per search endpoint)
  is pinned in-repo, and a `schema_canary` test validates the search fan-out's
  declared filter set (`search_supported`) against it at compile time
  (`include_str!`). A filter nbox sends that the pinned release doesn't accept
  fails the build naming the exact endpoint + filter — the manual
  schema-curling this session relied on, replaced by a reproducible signal.
  Refresh the snapshot against a new NetBox release with
  `scripts/gen_schema_snapshot.py` (from a saved `/api/schema/` JSON or a live
  URL) and the canary flags the drift before it reaches users. Centralizing the
  per-endpoint filter lists into `search_supported()` also made it the single
  source of truth the canary validates (no more scattered `&[...]` literals).

- **`nbox serve --print-config` (install recipes).** Prints the paste-ready
  `mcpServers` JSON object most MCP hosts read, then exits — without starting
  the server or connecting to NetBox, so it works before a token is even set.
  The `command` is the absolute path to this binary (the host finds it even if
  `nbox` isn't on its `PATH`); `args` always begins with `serve` and echoes any
  `--profile`/`--config` you passed so the snippet reproduces your invocation;
  `env.NBOX_TOKEN` is a placeholder (set it there, export it, or drop the block
  if `nbox config init` holds the token). docs/MCP.md now lists the exact
  config-file path per host (Claude Code, Claude Desktop, Cursor). This prints
  the stdio recipe; the HTTP/OIDC transport is configured separately.

- **`rack-group` + `vm-type` kinds (NetBox 4.6).** Two new first-class lookups
  for the 4.6 additions: `nbox rack-group <slug|name|id>` and
  `nbox vm-type <slug|name|id>`, each a full kind — `nbox_get` (MCP),
  `nbox journal <kind> <ref>`, `nbox open <kind>/<ref>`, the
  `nbox://<kind>/<ref>` resource, and a `nbox search` fan-out. Both are
  dead-simple name/slug/description objects with a relation count (`rack_count`
  / `virtual_machine_count`), plus `owner`/`tags`/`custom_fields`. Model shapes
  verified against the live 4.6.2 OpenAPI schema. (`cable-bundle`, the third 4.6
  addition, is deferred — it pairs with the cable-path visualizer.)

- **`owner` field + `--owner`/`--owner-group` filters (NetBox 4.5).** NetBox
  4.5 added a native `owner` (a user or group) on most objects — structured
  ownership that beats tag-scraping for agents. `owner` is now surfaced on every
  detail view as a friendly label, omitted when absent (byte-identical for
  pre-4.5 objects). In `search`, `--owner <name>` and `--owner-group <name>`
  filter by user/group name; owner is polymorphic (user **or** group) so the two
  are separate filters, and both are silently ignored on releases that carry no
  owner data (every owner-bearing search endpoint accepts the params; nbox never
  sends a filter a kind can't satisfy).

- **`virtual-circuit` kind (NetBox 4.2+).** `nbox virtual-circuit <cid|id>`
  shows a virtual circuit and its terminations, and it's a full first-class kind:
  `nbox_get kind=virtual_circuit` (MCP), `nbox journal virtual-circuit <cid>`,
  `nbox open virtual-circuit/<cid>`, the `nbox://virtual_circuit/<cid>` resource,
  and a `nbox search` fan-out. Virtual circuits are multi-point overlays on
  device interfaces — no A/Z sides, no physical cables — so each termination is a
  flat `device`/`interface` ref (for navigation) rather than a cable-path
  diagram. The view also surfaces the optional `provider_network`,
  `provider_account`, `type`, `status`, `tenant`, `owner` (4.5), and
  `description` scalars. Model shape verified against the live 4.6.2 OpenAPI
  schema.

- **MCP response contracts: every `nbox_get` kind view pinned.** The JSON shape
  each `nbox_get` kind returns is now pinned by a response-contract test
  (`src/mcp/tests.rs::contracts`): a removed/renamed field, or a new one not
  listed, fails the test. Covers every `nbox_get` kind plus the already-pinned
  status/search/journal/tags/tagged/available/cache-clear reports. Closes the
  remaining contract gap; zero runtime change.

- **Interface journal + `nbox_get interface` (MCP).** Interfaces are now a
  first-class kind in the journal resolver and `nbox_get`. `nbox journal
  interface <device>/<name>` surfaces an interface's operator notes like every
  other kind, and `nbox_get` (MCP tool + `nbox://interface/<device>/<name>`
  resource) returns the same `InterfaceView` as `nbox interface`. Interfaces
  have no single-string reference — they're addressed by device + name — so the
  compound `<device>/<name>` form is the ref, taken verbatim after the device
  since names may contain slashes (`xe-0/0/1`, `Ethernet1/49`). The `nbox open
  interface/<device>/<name>` path now shares the same splitter.

- **NAT inside/outside enrichment (`nbox ip`).** NetBox 4.6 embeds `nat_inside`
  (a brief IP ref) on a NAT *outside* IP and `nat_outside` (an array) on the
  *inside* IP. `nbox ip` now surfaces both: the outside IP shows its inside
  address (`nat_inside: …`), the inside IP shows its outside address(es)
  (`nat_outside: …`). Both are omitted when absent, so a non-NAT IP's output is
  byte-identical to before. The device IP tab (which fetches full IPs) picks it
  up for free.

- **Reverse-tag lookup (`nbox tagged <tag>`).** A new command that answers
  "what objects carry tag X" across all kinds in one call, via NetBox 4.3's
  `/api/extras/tagged-objects/?tag_id=<id>`. Distinct from `search --tag`
  (which needs a `q` and filters per-endpoint). The tag resolves by id, exact
  name, or exact slug (names may carry colons, e.g. `prod:us-east`); a no-match
  tag is not-found (exit 4). Each result carries a friendly `kind`/`object_type`
  (mapped to nbox's kind labels, with a derived fallback for types nbox doesn't
  model), the object id/display/url, plus the resolved tag. Available on the CLI
  (`nbox tagged`) and via MCP (`nbox_tagged`). The `TaggedReport` JSON shape is
  pinned by a response-contract test (top-level `tag`/`results`, row
  `kind`/`object_type`/`id`/`display`/`url`).

- **MAC reverse-lookup (`nbox mac <addr>`).** A new first-class kind (NetBox
  4.2+) that reverse-resolves a MAC address to the interface(s)/device(s) that
  carry it — a top operator/agent query nbox couldn't answer. Any common MAC
  form is accepted and normalized (`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF`,
  `aa-bb-…`, `aabbccddeeff`, a trailing `/48` is stripped); a non-MAC is a usage
  error (exit 2) with no NetBox round-trip. MACs aren't enforced globally unique,
  so several carrying interfaces surface as ambiguous (exit 5) with the candidate
  list, not a silent first-pick. Available on the CLI (`nbox mac`), via MCP
  (`nbox_get` kind `mac` / `nbox://mac/<addr>`), and `nbox open mac/<addr>`.
  Polymorphic assignment (physical interface *or* VM interface) is rendered as
  `"<parent> <interface>"`.

- **CIDR-containment filter for prefix/IP browse.** From the Nav rail, `/` on a
  prefix browse now filters **server-side** by network containment (`within_include`
  — the prefix + everything inside it) and on an IP-address browse by `parent`
  (addresses inside the prefix), instead of falling back to global search. The value
  is a CIDR, validated locally on Enter — a typo is an instant error, not a NetBox
  400 round-trip; the pane title reads `within "10.0.0.0/24"`. Completes the browse
  filter across every Nav-rail kind (name-bearing kinds by `name__ic`, prefix/IP by
  containment); the router's `None` → search fallback remains for any future
  non-filterable browse kind.

- **Credential preflight in `nbox status` (NetBox 4.5+).** `/api/status/` is
  reachable without a valid token on instances with `LOGIN_REQUIRED=False`, so a
  200 status response can hide a bad/expired token. `nbox status` and MCP
  `nbox_status` now run a best-effort probe of NetBox 4.5's
  `/api/authentication-check/` (gated on `IsAuthenticated`; returns the flat
  `UserSerializer` body) and surface the verdict in a new `token` field: `valid`
  (carrying the authenticated `username`/`display`), `invalid` (HTTP 401/403 — the
  token was rejected, with the server's reason), or `unverified` (the endpoint is
  absent on NetBox < 4.5, or the probe could not run). It never errors, so it can't
  turn a successful status fetch into a failure, and the exit-code contract for
  `nbox status` is unchanged: a rejected token during the status fetch still exits
  3; the preflight is informational. The capability probe and the preflight now
  overlap (`tokio::join!`), so `nbox status` costs no extra serial round-trip for
  the token verdict.

### Fixed

- **`nbox search --tenant` no longer over-broadens rack-groups / VM types.**
  The 4.6 kinds `rack-group` and `vm-type` have no `tenant` filter in the
  NetBox OpenAPI schema, but the search fan-out declared one for them — so with
  `--tenant` active those branches sent a `tenant=` param NetBox silently
  ignores and returned the kind *unfiltered* (mixed in with the tenant-scoped
  hits). They now declare only the filters the schema accepts (`tag`/
  `owner`/`owner_group`) and are skipped when `--tenant` is active, matching
  every other scope-mismatched branch. Caught by the new schema canary.

### Changed

- **`list_all` follows the server's `next` link across pages.** Pagination no
  longer computes `offset += page_size` itself; from the second page on it
  follows the `next` URL NetBox (DRF `LimitOffsetPagination`) returns, which
  echoes every original filter param and sizes its offset with the *capped*
  limit. This fixes a silent row-skip on NetBox servers configured with
  `MAX_PAGE_SIZE` below the requested limit (the short page advanced by the full
  request size, overshooting the rows the server actually returned) — the gap
  can no longer open. Default NetBox (`MAX_PAGE_SIZE` ≥ 1000) was unaffected and
  sees no behavioral change; single-object lookups are unaffected. Output is
  byte-identical on every path that was already correct.

- **Prefix CLI/MCP detail fetch is now concurrent.** The shared `prefix_view_by_ref`
  path (the CLI `nbox prefix` and MCP `nbox_get` prefix arm) fetched the prefix's
  children and member IPs in two sequential awaits; it now mirrors the TUI detail
  arms with one `tokio::try_join!`. A prefix detail costs one round-trip for the
  header plus one for both child collections (byte-identical output, no new
  backend), instead of two serial child fetches.

## [0.11.0] - 2026-06-23

### Added

- **Browse filter (grep-style).** When browsing a name-bearing kind from the Nav
  rail, `/` now filters that list **server-side** by name instead of opening the
  global search: type a substring, Enter, and the list re-fetches matching rows.
  Explicit (not live), so it doesn't hammer NetBox while you type. Uses the kind's
  case-insensitive name lookup (`name__ic` for devices/racks/sites/VLANs/VRFs/
  route-targets, `cid__ic` for circuits). The pane title shows the active filter and
  count (`Devices · name contains "edge" · 52`), `1000+` signals the result cap (refine
  to narrow). `Esc` on the list clears the active filter; while editing, `Esc`
  instead cancels the edit (keeping the filter), and `Ctrl+X` or an empty Enter
  clear it.
  This makes large instances usable — finding one of hundreds of thousands of
  devices by name instead of scrolling a capped list. Prefix and IP-address lists
  keep `/` as global search: their key field is a CIDR/inet column with no NetBox
  substring lookup, so a name-style filter there would silently match nothing it
  claimed to (CIDR-containment filtering for those kinds is planned separately).

### Changed

- **Browse cap raised 500 → 1000.** A Nav-rail browse now pulls up to 1000 rows
  (was 500). A cap-full list stays a single round trip — 1000 is NetBox's
  per-request ceiling, so `list_all` sizes one page to it; the filter narrows
  past the cap. The list-count badge reads `1000+` when capped.

## [0.10.0] - 2026-06-23

### Added

- **Circuit terminations + A↔Z path.** `nbox circuit <cid>` now resolves the
  circuit's A/Z terminations and renders the path: each side's endpoint (a site or
  a provider network) and the cable chain to the device it lands on — **walking
  through patch panels** (rear↔front) to the far interface, since NetBox exposes no
  `/trace/` for circuit terminations. Drawn as a vertical A↔Z diagram (mirroring the
  interface cable-path view); the A and Z sides are walked concurrently, and a hop
  that can't continue (e.g. an unwired panel) stops cleanly rather than guessing.
  The TUI circuit detail gains a **`p` path tab** and navigable links to the
  provider, the sites, and every device along the path. `-o json` (and the MCP
  `nbox_get` / `nbox://circuit/{ref}` resource) carries a structured `terminations`
  array — each hop in its `path` includes a `device` ref (`{id, name}`) so an agent
  can jump straight to the device — plus the rendered `diagram` lines so an agent
  or script can show the A↔Z art verbatim. Previously a circuit showed only flat
  attributes (provider/type/status/rate) with no indication of where it landed.
  Commit/port rates are humanized (e.g. `400 Gbps`).
- **`nbox profile remove <name>`** deletes a profile from the config
  (format-preserving). It refuses to remove the active profile (switch with `nbox
  profile use <other>` first) or the only profile, mirroring the TUI's delete
  guards — so you can drop a stray profile (e.g. one left by the onboarding
  wizard) without hand-editing the file.

### Changed

- **First-run onboarding wizard redesign.** The wizard now leads with the URL and
  shows a simple three-field screen (url / name / token); the niche knobs
  (`token_env`, `auth_scheme`, `verify_tls`) tuck behind a `Ctrl+A` "advanced"
  toggle. The profile **name is no longer hardcoded to `default`** — it's derived
  from the URL host (`https://netbox.acme.com` → `acme`, falling back to `prod`
  for an IP/empty host), shown live as a placeholder and committed on save unless
  you type your own. This stops the wizard from planting a stray `default` profile
  that duplicates one you add later.

### Fixed

- The `--no-tui` first-run setup hint printed the wrong `profile add` syntax
  (`--url <url>`); the URL is a positional argument. The hint now matches the CLI
  and the docs: `nbox profile add <name> <url> [--token-env <VAR>]`.

## [0.9.0] - 2026-06-22

### Added

- **Interfaces are first-class in the TUI.** A device detail's interfaces (`i`) and
  cables (`c`) tabs are now selectable lists — `j`/`k` move a cursor and `Enter`
  opens the interface's detail (its attributes plus the cable path). A new
  `interface` object kind backs this purely as a navigation/detail target: it's
  reached from a device, not the global search fan-out, so `nbox search` / MCP
  `nbox_get` keep their existing surface, and the new navigation row `id`s are
  non-serialized — the `nbox device` JSON/CSV *shape* is unchanged (the cable
  endpoint *labels* gain the far device; see the Changed note below).
- **Cable-path visualizer.** An interface's cable trace renders as a vertical A↔Z
  diagram — the near end on top (`A`), the far end on the bottom (`Z`), the device
  emphasized over its port, and each cable segment labelled (`#id · type · length ·
  status`). A patch panel in the path collapses to a single pass-through stop
  (`front → rear`); an unterminated side is shown explicitly. It's a dedicated
  "cable path" tab in the TUI and an inline section in `nbox interface` output.
- **Cable views name the far device.** The Cables tab and an interface's "Connected
  To" now show the remote *device* and port (e.g. `swp25 → edge01 1/1/c13/1`),
  not just the remote port — so you can tell where a cable goes.

### Fixed

- **`nbox raw GET` accepts an API path with or without the `/api/` prefix.** A bare
  path like `dcim/devices/?limit=1` previously resolved against the base URL to the
  *web UI* (`https://host/dcim/devices/`), which returns HTML and failed to decode
  ("expected value at line N"). The path is now normalized to `/api/…` whether you
  write `dcim/...`, `api/dcim/...`, or `/api/dcim/...`; absolute URLs are rejected so
  `raw` stays scoped to the active profile.
- **Nav-rail per-kind counts no longer clip on large instances.** The browse rail
  rendered the full count right-aligned in a too-narrow fixed-width pane, so a big
  number was cut to its first digit(s) (`● Devices 3` for 302142, `Prefixes` with no
  count at all). Counts are now abbreviated — exact below 1000, then `k`/`M` (`30k`,
  `2.1M`) — and the rail is widened to fit the longest label plus that count.
- **Detail tabs that list related objects now show their selection cursor.** Device,
  prefix, VLAN, site, and rack details carried navigable tab rows but rendered them
  as plain text with no cursor — only the VRF/route-target views (which have a
  header card) drew the cursor. The tab bar now pins in a fixed band for any tabbed
  detail and every section renders its rows the same way, so the device
  IP/VLAN/interfaces/cables, prefix children/addresses, and site/rack device tabs
  are all actually navigable now.

### Changed

- **Cable endpoints now name the far device, in every surface.** The Cables tab and
  `nbox interface` render endpoints as `local-port → far-device far-port` (was
  `local-port -> far-port`, remote device omitted). The far device also flows into
  the serialized `connected_to` labels, so `nbox interface` / `nbox device` JSON and
  MCP `nbox_get` now carry `"<device> <port>"` there — a content change to those
  string labels, not a shape change.

### Security

- Updated the transitive `quinn-proto` dependency to `0.11.15` to clear
  RUSTSEC-2026-0185. It's a lockfile-only dependency (pulled via `reqwest`); nbox
  builds without QUIC/HTTP3, so it isn't compiled into the binary — but the release
  audit gate scans the whole `Cargo.lock`.

## [0.8.1] - 2026-06-22

### Fixed

- **Browsing sites no longer times out on large instances.** NetBox's full site
  list serializer attaches per-site aggregate counts (device / prefix / rack / vlan
  / circuit), each a subquery over a large table — slow enough to exceed the request
  timeout on a sizable instance (observed: 100 sites > 120s, while the nav count and
  every other browse kind return in well under a second). The site browse now
  requests NetBox's `brief` representation, which omits those counts and returns the
  `name` + `slug` the browse index shows (~400× faster in testing). Opening a site
  still fetches the full object for its detail view, so nothing is lost there. Only
  the site browse is affected; the other kinds already list quickly and keep their
  full columns.

## [0.8.0] - 2026-06-22

### Changed

- **The OS keyring is gone — the API token lives in `config.toml` or an env var,
  full stop.** A token pasted in the first-run wizard or the Settings profile
  builder is saved to `config.toml` (`token = "..."`, `0600` on Unix, redacted from
  `config show` / `--json` / `Debug`) and you're connected. Resolution precedence is
  `token_env` → `NBOX_TOKEN` → config token → none, with each source normalized (a
  pasted `Bearer `/`Token ` prefix or stray whitespace stripped) *before* it
  competes — so a high-precedence source that's set but blank (e.g.
  `NBOX_TOKEN="Bearer "`) can't mask a valid lower one. This removes the 0.7.2
  opt-in keychain entirely: the `keyring` dependency and Cargo features,
  `nbox config token set`/`clear`, the TUI `Ctrl+K` toggle, and the `token_store`
  config key are all gone. `nbox config token status` still reports the resolved
  *source* (never the value).

  **Migration:** if you previously opted a profile into the keyring
  (`token_store = "keyring"`), re-enter its token in the TUI Settings profile editor
  (or add `token = "..."` under `[profiles.<name>]` in `config.toml`), or point it at
  a `token_env` / set `NBOX_TOKEN`. The `token_store` key is now ignored; you can
  delete it and any orphaned entry from your OS keychain app.

- **`nbox config init` now creates `config.toml` owner-only (`0600` on Unix).**
  Since the token can live in the file, a freshly-created config is locked down up
  front — before you uncomment/add `token = "..."` — matching the permissions used
  when the TUI writes a token.

### Fixed

- **Pasting a token with a `Bearer `/`Token ` prefix now works.** NetBox's UI
  "copy" button hands you the full `Authorization` header value (`Bearer nbt_…`);
  nbox now strips a leading scheme word (and stray whitespace) from any token
  source — the config token, `token_env`, or `NBOX_TOKEN` — and adds the scheme
  itself from `auth_scheme`. Already-saved configs with a prefixed token start
  working on the next run. The TUI/onboarding `Ctrl+T` test-connect resolves its
  token through the same normalized precedence, so a test result matches what a real
  connection will do.
- **Auth errors now show NetBox's reason.** A 401/403 surfaces the server's
  `detail` (e.g. *"permission denied (HTTP 403): Invalid v2 token — check the token
  or permissions for this profile"*) instead of a generic message, so a bad or
  expired token is obvious at a glance.

## [0.7.2] - 2026-06-22

### Changed

- **Pasting an API token now just works — OS keyring storage is opt-in.** A token
  pasted in the first-run wizard or the Settings profile builder is saved to
  `config.toml` (`token = "..."`) and you're connected — no OS keychain prompt.
  This replaces the 0.7.1 behavior that blocked a pasted token unless a persistent
  keyring backend was available. The config token is redacted from `config show` /
  `--json` / `Debug`, and the file is written with `0600` permissions on Unix.
- **Opt into the OS keyring** with `Ctrl+K` in the TUI or `nbox config token set`:
  that sets `token_store = "keyring"`, moves the token into the keychain, and
  clears it from `config.toml`. The keyring is only ever consulted for profiles
  that opt in.
- Token resolution precedence is now `token_env` → `NBOX_TOKEN` → config token →
  (opt-in) keyring → none.

## [0.7.1] - 2026-06-22

### Fixed

- **First-run onboarding no longer freezes after you add a profile.** The wizard
  left a terminal-event reader running after it handed off to the app, so the
  first keypress in the freshly-launched TUI was swallowed (it felt stuck, and you
  had to force it closed). The reader is now cancelled at the handoff.
- **Pasted tokens are no longer silently lost when there is no OS keyring.** On a
  build without a persistent keystore (the default Linux/musl build, or any machine
  with no Secret Service backend), a pasted token went into a throwaway in-process
  store and vanished — so the app launched unauthenticated. Onboarding and the
  profile editor now block that save with clear guidance to use `token_env` /
  `NBOX_TOKEN` instead. macOS/Windows and Secret-Service Linux builds store pasted
  tokens as before.
- **Profile token saves are now transactional.** The keyring change is prepared
  before `config.toml` is written and rolled back if the write fails, so a save can
  never leave a profile without its token or a token orphaned under a profile that
  never landed.
- **Renaming a profile no longer fails when the keyring is unavailable.** The
  metadata rename always proceeds; if a stored token could not be migrated to the
  new name, you get a best-effort warning to re-enter it rather than a hard error.

## [0.7.0] - 2026-06-21

### Added

- **First-run orientation on the home screen.** A fresh launch (or any time there
  are no recent items yet) now lands on an oriented getting-started panel — `/`
  search, `j/k ↵` browse a kind from the rail, `D` dashboard, `T` prefix tree, `?`
  help — instead of a bare "Press / to search". Returning users with recents are
  unaffected.
- **Connection status at launch, and a recoverable connection banner.** A
  successful start shows a `connected to NetBox vX.Y` confirmation in the footer. A
  connection or auth failure (bad token, unreachable host) no longer hard-exits
  before the TUI opens — it launches with an actionable banner ("Press S to edit
  the profile or set NBOX_TOKEN"), so you fix the profile in-app and reconnect
  without re-running the binary. A server below the supported NetBox floor stays a
  hard, explicit error.
- **Edit the active connection from the Settings modal.** The Config modal's
  Settings section gains a **Connection** category for the active profile's
  `page_size`, `timeout_secs`, `exclude_config_context`, and the per-surface
  `[api]` `vrf` / `route_target` backends (`rest`/`graphql`). Saving a change
  persists it to the profile (format-preserving) and reconnects so it takes effect
  live. The profile editor still manages every profile; Settings is the quick-tweak
  surface for the active one.

### Performance

- **Prefix detail loads its child prefixes and contained IPs concurrently** — one
  combined fetch after the prefix header instead of two sequential round-trips.
- **Search resolves the `--scope` and `--vrf` references concurrently** before the
  multi-endpoint fan-out, so a scoped + VRF-filtered search pays one resolution
  tail instead of two.

### Internal

- Deepened machine-facing contract tests: strict response-shape pins for the MCP
  `nbox_next_ip` / `nbox_next_prefix` / `nbox_cache_clear` / `nbox_journal` /
  `nbox_list_tags` tools, and CSV RFC-4180 quoting verified through the compiled
  binary.
- Refactors with no behavior change: bundled the profile-edit-form arguments
  (`ProfileFormData`), shared one HTTP 429 retry policy across the REST and GraphQL
  paths, and slimmed `persist_profile`'s signature.
- Dependency maintenance: `sha2` 0.11, several GitHub Actions bumps, and the Rust
  toolchain pinned to the project MSRV (1.88) so CI keeps a low support floor.

## [0.6.0] - 2026-06-21

### Added
- **`nbox_get` (MCP) accepts `ip_address` as an alias for `ip`.** A `nbox_search`
  result's `kind` is `ip_address` (the one kind whose spelling differs from
  `nbox_get`'s `ip`; all others already match), so a search → get chain — and the
  `nbox://ip_address/<ref>` resource URI — now works without translating the kind.
- **Edit more profile knobs from the in-app Config modal.** The profile add/edit
  form now sets the settings that used to need hand-editing `config.toml`:
  `timeout_secs` and `page_size` (numeric fields; empty = default),
  `exclude_config_context` (Ctrl+E), and the per-surface API backends
  `[profiles.<name>.api] vrf` (Ctrl+B) / `route_target` (Ctrl+R), each cycling
  `rest`/`graphql`. REST backends and default/empty values leave the file clean
  (no `[api]` table, no redundant keys), and writes stay format-preserving. The
  API token is still never written to `config.toml`.
- **Drill into a prefix's children and contained IPs from the TUI.** The prefix
  detail's child-prefix and IP-address lists are now navigable tabs (`c` children,
  `a` addresses): select a row and press Enter to open that prefix or IP, with
  `b`/`Esc` walking back through the drill path. The `nbox prefix` CLI/JSON output
  is unchanged.
- **Drill into a device's IPs and VLANs from the TUI.** The device detail's
  IP-address (`p`) and VLAN (`v`) tabs are now navigable — Enter opens that IP or
  VLAN, `b`/`Esc` walks back. (Interfaces, cables, and services stay plain text;
  they aren't standalone objects.) The `nbox device` CLI/JSON output is unchanged.
- **Drill into a VLAN's prefixes from the TUI.** The VLAN detail's prefix list is
  now a navigable `p` tab — Enter opens that prefix, `b`/`Esc` walks back. The
  `nbox vlan` CLI/JSON output is unchanged.
- **Drill into a site's / rack's devices from the TUI.** The site detail gains
  navigable `devices` (`d`) and `racks` (`r`) tabs, and the rack detail gains a
  navigable `devices` (`d`) tab alongside its elevation — Enter opens that device
  or rack, `b`/`Esc` walks back. These lists are fetched best-effort (a fetch error
  shows in the tab; the summary still loads).
- **The detail screen advertises its tabs and the drill action.** The tab bar now
  has a leading `tabs:` label with the active tab bolded, and the footer shows
  `Tab tabs · Enter open` — kind-agnostic (replacing the old device-only
  `i/p/c/v/s` hint), with `Enter open` shown only when the active tab actually has
  navigable rows (so it never lies on a summary/text tab). Makes the navigable
  detail lists discoverable instead of hidden behind an unmarked tab switch.

## [0.5.0] - 2026-06-20

### Added
- **Route-target view can use GraphQL.** Set `[profiles.<name>.api] route_target =
  "graphql"` to fetch a route target's importing + exporting VRFs in one `/graphql/`
  query instead of two REST `vrfs` list calls. Identity resolution stays REST (so
  not-found/ambiguous exit codes are unchanged) and the result is byte-identical to
  the REST path; an instance whose schema can't support it transparently falls back
  to REST, with the reason in `nbox status`. The surface joins the per-surface `api`
  block and the `capabilities` report. (Also fixes the GraphQL filter probe to
  introspect `RouteTargetFilter`, so the `id` filter is shaped correctly.)

### Changed
- **Browsing one kind shows kind-aware list columns.** Opening a kind from the Nav
  rail now drops the redundant per-row KIND tag — the pane title already names the
  kind — and labels the second column with the attribute that kind actually carries:
  `STATUS` for prefixes/IPs, `VID` for VLANs, `RD/TENANT` for VRFs, `TENANT` for
  route targets, `SITE` for devices/racks, `SLUG` for sites. The column sizes to its
  content. This replaces the fixed, often-empty `SITE` column, so a site-less kind no
  longer reads as a ragged, empty row. Mixed search results and Recent keep the
  `KIND / DISPLAY / SITE` layout.

## [0.4.0] - 2026-06-19

### Documentation
- **Docs overhauled to the project standard.** Restructured the README (a "vs the
  NetBox web UI / raw API" comparison table, a complete keybindings table, a
  troubleshooting section, a full documentation index) and added
  `docs/COMPARISON.md`, `docs/SCRIPTING.md`, and `docs/TROUBLESHOOTING.md`.
  Documented the in-memory read cache (`[cache]`) and the `nbox_cache_clear` MCP
  tool. Corrected the MSRV (1.88), the MCP tool count (nine), the searchable-kind
  lists (racks, VRFs, route targets), and the GraphQL/REST split (search is always
  REST; GraphQL backs the VRF view only) across every doc. Expanded `SECURITY.md`
  (the `nbox serve` network surface, supported-versions) and `CONTRIBUTING.md`
  (module map, an "adding a feature" recipe), and added GitHub issue/PR templates.

### Changed
- **BREAKING: per-surface API backends replace the coarse `backend` key.** The
  profile-level `backend = "rest"|"graphql"` setting is **removed**; a config that
  still sets it is rejected with a pointer to the new shape. Configure the backend
  per read surface under `[profiles.<name>.api]` instead:
  ```toml
  [profiles.work.api]
  vrf = "graphql"   # rest | graphql
  ```
  A missing table/key means REST; unknown surfaces (e.g. `detail`) and invalid
  values are config errors. REST stays canonical; a `graphql` surface is honored
  only when the live schema probe supports it, otherwise it **falls back to REST**.
  `nbox status` (CLI + MCP) drops the single `backend` field for a per-surface
  `api` block (`configured`/`effective`/`reason`), and `capabilities.graphql` is
  now surface-aware (`surfaces.{search,vrf}.{supported,recommended,missing}`).
- **Search is always REST.** NetBox's GraphQL API has no equivalent to REST's
  full-text `q` quick-search (filtering moved to per-field Strawberry lookups in
  4.3), so GraphQL can't reproduce canonical NetBox search. `nbox search` now
  always runs over REST; a `search = "graphql"` preference is accepted but
  transparently falls back to REST, with the reason in `nbox status`. The VRF view
  is currently the only GraphQL-capable surface. (The per-kind GraphQL search
  fan-out — which silently returned unfiltered results on 4.3+ — was removed.)

### Fixed
- **Search no longer randomly times out one endpoint.** NetBox is commonly served
  by gunicorn *sync* workers, which close the connection after each response;
  reqwest could reuse such a half-closed keep-alive connection from its pool and
  hang that request to the full timeout, so `nbox search`'s ~17-way fan-out would
  intermittently report one endpoint as failed (`operation timed out`) even though
  the server was healthy. nbox now disables idle-connection reuse (a fresh
  connection per request, like curl) and sets a 10s connect timeout.

### Performance
- **VRF detail (REST) fetches its prefixes and addresses concurrently** via
  `tokio::try_join!` instead of sequentially, roughly halving the REST VRF view's
  latency on a high-RTT link.

### Added
- **Live-browse the Nav rail.** Moving the Nav-rail cursor with `j`/`k` (or
  `g`/`G`) now auto-browses the highlighted kind into the results pane — no `Enter`
  needed — so scrolling the rail previews each kind's list (and its first item)
  beside it. It's debounced until the cursor settles, so a fast scroll doesn't
  flash the list of every section it passes; focus stays on the rail, and `Enter`
  still commits and jumps into the results. The footer reflects the rail's
  controls when it's focused (`j/k browse · Enter results`).
- **TUI remembers the last-browsed kind.** The Nav rail's browsed kind is
  persisted to `[ui].last_browsed` on exit and restored on the next launch — the
  cursor lands on it and its list preloads (focus stays on the Nav rail). First
  run (nothing remembered) still opens on Recent. Also: a **Route Targets** Nav
  section, right-aligned Nav counts, and a count on the Recent row.
- **Route targets are now a first-class object.** A route target (BGP extended
  community, e.g. `65000:100`) can be looked up (`nbox route-target <name|id>`),
  found in search, opened (`nbox open route-target/<ref>`), journalled, and
  fetched over MCP (`nbox_get route_target` / `nbox://route-target/<ref>`). Its
  detail is the relation graph — the VRFs that import and export it — built by
  resolving both directions over REST concurrently; each VRF row is navigable.
  The **VRF view's targets tab is now navigable**: import/export route targets
  are structured `{id, name}` (so `vrf --json` gains the id) and `Enter` opens the
  route target's detail, like the prefix/address sections.
- **VRF GraphQL bundle.** With `[profiles.<name>.api] vrf = "graphql"`, the VRF
  view fetches its prefixes + addresses in a single GraphQL query (the VRF
  identity is still resolved over REST, preserving not-found/ambiguous exit codes).
  REST and GraphQL produce a byte-identical `VrfDetail`. `nbox vrf` now prints the
  full routing context (summary + prefix tree + addresses), and MCP `nbox_get vrf`
  returns the same bundle.
- **VRFs are now a first-class object.** A VRF can be looked up (`nbox vrf <name|rd|id>`),
  found in search (`nbox search` / TUI `/` / MCP `nbox_search`, REST and GraphQL —
  subtitle = its RD, falling back to the tenant), browsed from the TUI Nav rail
  (a **VRFs** section with a live count), opened from the palette (`vrf <ref>`),
  resolved by `nbox open vrf/<ref>`, journalled (`nbox journal vrf/<ref>`), and
  fetched over MCP (`nbox_get` / `nbox://vrf/<ref>`). The VRF view normalizes RD,
  tenant, enforce-unique, import/export route targets, and the prefix/address
  counts. In the TUI the detail opens as a routing context: a fixed header card
  (RD · tenant · route-target counts · enforce-unique) over the VRF's prefix tree,
  with `addresses` and `targets` tabs. The prefix and address rows are navigable —
  `j`/`k` move a cursor and `Enter` opens that prefix/IP (`b`/`Esc` returns), the
  same drill the related-objects (`R`) jump performs. Previously VRF was only a
  search *filter* (`--vrf`) and an exact-by-RD lookup, never a navigable object.
- **Navigable detail sections.** Detail tabs can now be interactive lists (a row
  cursor with `Enter` to open), not just scrollable text — the foundation the VRF
  view's prefix tree and address list are built on. Sections without navigable rows
  scroll exactly as before.
- **Three-pane home (Navigation rail).** The home screen is now Nav │ Results │
  Detail. The left Nav rail browses by kind — Devices / Prefixes / IPs / VLANs /
  Sites / Racks — each with a domain-colored bullet and a **live object count**,
  plus a Recent entry. `Enter` lists a kind into Results (search stays on `/`); the
  Results title names the kind. `Tab` / `Shift-Tab` cycle the three panes.
- **Rack elevation.** A rack's detail now has an `e` (elevation) tab: a framed,
  U-by-U front view (from NetBox's `/dcim/racks/{id}/elevation/` endpoint) where
  each device fills its U span with the name on the top row, and any devices
  assigned to the rack without a mounted position are listed below as "not racked".
  Reachable via `e` or by cycling detail tabs with Tab.

## [0.3.0] - 2026-06-19

### Added
- **TUI update banner.** When the background update check finds a newer release, the
  TUI shows a dismissible (`u`) banner across the top with the install-appropriate
  upgrade command — parity with the CLI notice, which already printed one. Help and
  the banner both note the `u` dismiss key.
- **Racks are now searchable.** `nbox search` / the TUI `/` search / MCP `nbox_search`
  fan out to `dcim/racks/` (REST and GraphQL backends), so a rack surfaces as a
  ranked result (subtitle = its site) you can open like any other kind. Racks honor
  the `status`/`tenant`/`role`/`tag` filters and the site/region/site-group/location
  scope (by resolved `*_id`, like devices). A `rack <name|id>` palette lookup was
  added too. Racks were previously CLI-only (`nbox rack <ref>`) and a drill-only
  TUI target.
- Profile-level GraphQL search backend. Set `backend = "graphql"` on a profile to
  run `nbox search` through NetBox's `/graphql/` endpoint while keeping REST as
  the default and as the backend for detail lookups, journals, raw reads, and
  available-IP/prefix commands. The GraphQL path probes the schema at runtime and
  adapts to NetBox 4.2 unpaginated list fields, NetBox 4.3+ offset pagination,
  and NetBox 4.5+ lookup-wrapper filters for IDs/enums. Probed capabilities are
  cached per client and shared across clones, so repeated TUI searches do not
  re-run introspection. GraphQL pagination is capped at NetBox's maximum page
  size, and list decode errors include the GraphQL list name for easier debugging.
- Settings now cover **`log_level` and `log_file`** (a new *Logging* category in the
  Config modal). Set the tracing filter (e.g. `nbox=debug`) and a log-file path from
  the TUI; both persist to `config.toml` (format-preserving) and apply on the next
  launch (tracing initializes at startup).
- **TUI search filters.** The TUI now applies the same filters as the CLI search —
  `status` / `site` / `region` / `site-group` / `location` / `tenant` / `role` /
  `tag` / `vrf` — via the command palette: `filter status=active site=dc1`,
  `unfilter <key>`, and `filter` (or `clear-filters`) to clear. Filters ride every
  search through the existing resolver (scope mutual-exclusion, VRF-by-ref,
  per-endpoint allowlist), so unknown keys are rejected and the TUI never sends an
  unknown query param. Setting a filter re-runs the last query. Active filters show
  as a **chips bar** above the results (scope filters in the header color, the rest
  in the accent), so what's applied is always visible. A discoverable **`f` filter
  modal** edits them all (the four scope filters collapse into one type+value row,
  so only one scope is ever set), and **`F`** clears every filter.
- **Clear the search** from the TUI: `Esc` on the home screen (when results are
  showing) clears the search back to the recents list, and the palette
  `clear-search` (alias `clear`) does the same — the counterpart to `F`. `b` stays
  plain back-navigation.
- **Overview dashboard** (TUI). Press `D` for a read-only landing screen: device
  counts by status, the most-utilized prefixes (with utilization bars), and recent
  journal activity — fanned out concurrently. `r` refreshes, `b`/`Esc` returns
  home. Utilization ranking is best-effort over a capped page (NetBox has no
  `ordering=utilization`).

### Changed
- The TUI profile switcher (`P` / `Ctrl+P`) now cycles profiles in **config-file
  order** instead of alphabetical. Profiles are loaded into an order-preserving
  map (`indexmap` + `toml`'s `preserve_order`), so `[profiles.*]` keep their TOML
  document order everywhere they're listed (`profile list`, `config show`, and the
  switcher). No config change needed.
- The update check now hits GitHub **at most once a day** (disk-cached via
  update-informer) instead of a network round-trip on every invocation, and
  recognizes a **container** install — suggesting `docker pull ghcr.io/lance0/nbox`
  alongside the existing Homebrew / Cargo / downloaded-binary upgrade hints.
- TUI header and footer now render as proper status bars: a subtle per-theme
  background fill (`chrome_bg`, added to every theme), the profile emphasized with
  the NetBox URL/version dim and the mode right-aligned in the header, and the
  footer nav hints with accented keys + dim labels. Cosmetic only.
- TUI list, preview, and detail panes now have one column of inner padding, so
  their content no longer touches the pane borders. Cosmetic only.
- The Config modal's Settings section is now a **two-pane categories ▏ fields**
  layout (Appearance / Behavior / Logging): `↑/↓` pick a category, `→` enters its
  fields, `Esc` steps back, `Enter`/`Ctrl+S` save. Scales as settings grow without
  a cramped single column.
- TUI results table polish: the **KIND** column is now colored by NetBox domain
  (hosts / addressing / locations / circuits / tenancy) so it's scannable, and the
  selected row uses a solid gutter bar (`▌`) instead of `>`. Cosmetic only.
- TUI context preservation: a detail's **tab + scroll position are remembered per
  object**, so re-opening (or refreshing) something you've already looked at
  restores where you were instead of snapping back to the summary at the top. (The
  home cursor, active filters, and the loaded dashboard were already kept across
  navigation.)
- The update notifier now ships in the **default** build, so a released binary
  tells you when a newer version is available. It checks GitHub on a background
  thread, only on a TTY and never in `--json`/piped output, so scripts are
  unaffected; `--no-default-features` still opts out. (Part of shipping one
  canonical full-featured binary per platform.)
- **MSRV lowered to Rust 1.88** (was 1.95). The 1.95 floor was a leftover from the
  (since-removed) on-disk cache feature's `rusqlite`/`libsqlite3-sys`; the only
  remaining things forcing 1.95 were two `Duration::from_mins(1)` calls, now
  written as `from_secs(60)`. nbox now builds on the floor the `let`-chains set
  (1.88). Release builds derive the canonical feature set from `default` (the
  redundant `--features updates` is gone — `updates` is already a default feature).

### Fixed
- TUI footer/navigation UX: theme changes now apply visually without replacing the
  bottom navigation bar with a sticky `theme: ...` message. Live state now owns
  the far-left footer slot (spinner, result counts, errors, theme notices), with
  context-specific navigation hints following it; transient theme notices clear
  back to the nav-only resting state.
- TUI search/command line: the `/` and `:` input is now inset one column from
  each edge, so its sigil aligns with the header and the `/ search` hint instead
  of hugging the terminal's left edge.
- TUI detail actions: `o` and `y` now target the loaded detail object on the
  Detail screen instead of falling through to the hidden Home selection.
- Docs referenced a removed `cache` build feature (`cargo install nbox --features
  cache,...`) that no longer exists and would fail; the README and CONTRIBUTING
  now document the actual single-binary feature set.

## [0.2.0] - 2026-06-18

### Added
- First-run onboarding wizard (TUI). Launching `nbox` with no usable config — no
  config file, no profiles, or no resolvable active profile — no longer dies with
  "run `nbox config init`"; it opens a guided wizard that captures one profile
  (name, url, token or `token_env`, `auth_scheme`, `verify_tls`), reusing the same
  add-form and `Ctrl+T` test-connect (`verify_compatible`) as the Config modal's
  profile editor. `Enter` saves the profile (written + made active in
  `config.toml`, format-preserving) and drops straight into the normal TUI; `Esc`
  (or `Ctrl+C`) quits cleanly without writing anything. A pasted token is stored
  in the OS keyring (never in TOML); when the keyring is unavailable the profile
  still saves and the app opens with a "set NBOX_TOKEN or a token_env" note. The
  wizard and the app share one terminal, so there's no re-init flicker.
- In-app Config modal with a profile editor (TUI). Press `S` (or run `config` in
  the command palette) to open a floating Config modal on its Profiles section:
  list the configured profiles (the active one marked), and add / edit / select /
  delete them without leaving the TUI or hand-editing `config.toml`. The add/edit
  form has fields for `name`, `url`, `token_env`, an `auth_scheme` cycle
  (auto/bearer/token), a `verify_tls` toggle, and an optional masked token field.
  A typed token is stored in the OS keyring on save (never written to TOML, never
  echoed); when the keyring is unavailable the profile metadata is still saved
  with a clear "set a token_env or NBOX_TOKEN" note that survives save+use
  reconnects. `Ctrl+T` test-connects the
  form (it rebuilds a temporary client and re-probes `/api/status/`, the same
  check launch runs) and shows success/failure before you commit; `Enter` saves,
  `Ctrl+G` saves and switches to the profile. An explicit add/select **persists**
  `active_profile` to the file (the quick `P`/`Ctrl+P` cycle stays session-only).
  Delete drops the profile from the file, the keyring, and the live list, and is
  blocked for the active or last-remaining profile.
- Settings section in the Config modal (TUI). `Tab` switches the Config modal
  between Profiles and Settings; the Settings section is an editable form over the
  real `[ui]` settings: **theme** (cycle with `←`/`→`/Space — applied live as you
  cycle), **refresh_secs** (the TUI auto-refresh interval; empty/`0` = off), and
  **open_browser_command** (a custom browser-open command; empty = the OS
  default). `↑`/`↓` move between fields; `Enter` or `Ctrl+S` saves. On save each
  changed field is written to `config.toml` format-preserving (comments and other
  keys survive), the auto-refresh ticker re-arms at the new interval without a
  restart, and the new browser command takes effect on the next open. The no-op
  `wide` / `confirm_writes` knobs are intentionally excluded (both are
  parsed-but-unused today). `NO_COLOR` still wins: the theme change is disabled
  under `NO_COLOR`, the same as the `t` cycle and the palette `:theme` verb.
- `[ui].open_browser_command` is now honored. `nbox open <kind/ref>` and the TUI
  `o` open action run the configured command (split into program + args, with the
  URL appended as a literal final argument — never shell-interpolated) instead of
  the OS default opener; an empty value keeps using the OS default. The TUI reads
  the live value, so a command just changed in the Settings section applies to the
  next `o` without a restart.
- OS keyring token storage + `nbox config token set|clear|status`. `set` stores
  the active (or `--profile`) profile's NetBox API token in the OS keyring,
  reading it without echo from a TTY prompt — or as a single line from stdin when
  piped, for scripting. There is no positional token argument, so the token can't
  leak into shell history; it is never echoed, logged, or written to the config
  file. `clear` removes the stored token; `status` reports the resolved token
  *source* (`token_env`/`NBOX_TOKEN`/`keyring`/`none`) without ever printing the
  token. The keyring entry is keyed by config path + profile name (service
  `nbox`). macOS Keychain and Windows Credential Manager are built in; the Linux
  Secret Service (D-Bus) backend is opt-in via `--features keyring-secret-service`
  (keeping static/musl builds free of a D-Bus link dependency) — without it,
  `config token` reports the keyring as unavailable and steers you to an env var.
- `tags` on the remaining detail views, for consistency with the newer ones.
  `nbox device`/`site`/`rack`/`circuit`/`ip`/`prefix`/`vlan`/`interface`/
  `aggregate`/`asn`/`ip-range` now surface the object's tags — joined slugs as a
  `tags:` line in plain output, a `tags` array in `--json` — dropped when the
  object has none, exactly as tenant/contact/provider/vm/cluster already do. The
  wire models already carried `tags` except `Prefix`, which gained the field (an
  additive, `#[serde(default)]` `Vec<Tag>` matching its siblings). Read-only and
  additive to the `--json` shape; `--fields` consumers are unaffected.
- TUI profile switcher: cycle between the profiles in your config without
  restarting. `P` switches to the next profile, `Ctrl+P` the previous (wrapping
  at both ends), and the palette `profile <name>` (alias `prof`) verb jumps to a
  named one. Switching rebuilds the NetBox client for that instance and re-probes
  `/api/status/` off the render thread — reusing the same connect + version-floor
  check the TUI runs at launch — so the header flips to the new profile and its
  NetBox version; an unreachable/unsupported instance surfaces a clear error and
  leaves the UI usable. The old instance's results/recents/detail are dropped on
  switch and any in-flight old-profile search/detail responses are suppressed by
  the request-id guard, so a slow straggler can't repaint the new instance. With
  a single configured profile the hotkey is a graceful no-op. Session-only: it
  does not rewrite `active_profile` in the config (use `nbox profile use <name>`).
- Virtualization lookups: `nbox vm <name|id>` and `nbox cluster <name|id>`,
  read-only and additive. VM surfaces its status, role/cluster/device/platform
  (brief), vcpus, memory, disk, primary IPv4/IPv6, tenant, site, description,
  tags, and custom fields; cluster surfaces its type/group (brief), status,
  tenant, scope (the polymorphic `scope`/`scope_type` — site/region/…), non-zero
  device and VM counts, description, tags, and custom fields. Both render plain
  and `--json`. Neither carries a slug, so they resolve id → `name__ie` →
  `name__ic`; an ambiguous reference exits `5` with the candidates. Search now
  fans out to virtual machines and clusters (both honor `q=`/`--tag` and `--site`;
  id-based scope filters skip them), and the `nbox_get` MCP tool, the
  `nbox://{kind}/{ref}` resource template, `nbox open`, and `nbox journal` all
  gain `vm` and `cluster` kinds, routed through the same shared view layer as the
  CLI.
- MCP resources: the `nbox serve` server now advertises a `resources` capability
  and a single resource template, `nbox://{kind}/{ref}` (e.g.
  `nbox://device/edge01`, `nbox://ip/10.0.0.1`), so hosts that browse/attach
  resources — not just call tools — can pull object context. Reading one routes
  through the same shared view layer as the `nbox_get` tool and returns the
  object's JSON view as the resource contents; `kind`/`ref` follow `nbox_get`
  (the full device/ip/prefix/vlan/site/rack/circuit/aggregate/asn/ip_range/
  tenant/contact/provider set), with a `ref` containing `/` percent-encoded
  (e.g. `nbox://prefix/10.0.0.0%2F24`). It's a template, not a static list, so
  `resources/list` is empty (enumerating every NetBox object would mean walking
  the whole instance). Unknown kind, malformed URI, or an unresolved/ambiguous
  `ref` returns an `invalid_params` error, mirroring `nbox_get`. Works on both
  the stdio and HTTP transports. Read-only and strictly additive — the eight
  tools are unchanged.
- Provider lookup: `nbox provider <slug|name|id>`, read-only and additive,
  rounding out the circuits ecosystem alongside `nbox circuit`. Surfaces the
  provider's ASNs (brief list), accounts, description, non-zero `circuit_count`,
  tags, and custom fields; renders plain and `--json`. Resolves id → slug →
  `name__ie` → `name__ic`; an ambiguous reference exits `5` with the candidates.
  Search now fans out to providers (honors `q=` and `--tag`; id-based scope
  filters skip it), and the `nbox_get` MCP tool gains `kind=provider`, routed
  through the same shared view layer as the CLI. `nbox open provider/<ref>` and
  `nbox journal provider <ref>` work too.
- Tenancy lookups: `nbox tenant <slug|name|id>` and `nbox contact <name|id>`,
  read-only and additive. Tenant surfaces its group (brief), description,
  non-zero relation counts (devices, prefixes, sites, …), tags, and custom
  fields; contact surfaces title, phone, email, address, link, group, tags, and
  custom fields. Both render plain and `--json`. Tenants resolve id → slug →
  `name__ie` → `name__ic`; contacts (no slug) resolve id → `name__ie` →
  `name__ic`; an ambiguous reference exits `5` with the candidates. Search now
  fans out to tenants and contacts (both honor `q=` and `--tag`; id-based scope
  filters skip them), and the `nbox_get` MCP tool gains `kind=tenant` /
  `kind=contact`, routed through the same shared view layer as the CLI. `nbox
  open tenant|contact/<ref>` and `nbox journal tenant|contact <ref>` work too.
- `search --vrf <id|rd|name>` server-side filter (and on the `nbox_search` MCP
  tool). The VRF reference is resolved once up front via `vrf_by_ref` (numeric
  id, then RD, then name — VRFs have no slug), then applied as `vrf_id=` on the
  VRF-capable endpoints (IP addresses, prefixes); endpoints that carry no VRF
  (devices, sites, VLANs, circuits, aggregates, ASNs, …) skip the filter rather
  than being dropped. An unknown VRF is a not-found error (exit `4`), not a
  silent empty result. Orthogonal to the `--site`/`--region`/`--site-group`/
  `--location` scope filters: both may be set, and NetBox ANDs them on prefixes.
  Reuses the same `--vrf` resolution as the `nbox ip`/`prefix` exact-lookup path.
- Operational layer for the HTTP transport (`nbox serve --http`): a structured
  audit log and a per-caller rate limit (completes the read-only HTTP/OAuth v1,
  DESIGN §24). Every authenticated request to `/mcp` emits one structured
  `tracing` event under the target `nbox::audit` — WHO (`sub`, `client_id`,
  `scope`, `jti`, `iss` in OIDC mode; the auth mode + peer IP in loopback /
  static-bearer mode), WHAT (HTTP method + path — the JSON-RPC/tool name is *not*
  surfaced, to avoid buffering the body and breaking the rmcp stream), WHEN
  (`request_id`, `session_id` from `Mcp-Session-Id`), and OUTCOME (status, a
  coarse `ok`/`auth-failed`/`rate-limited`/`error`, latency in ms). The token, the
  `Authorization` header, and secrets are never logged (the fields are an explicit
  allow-list); events log at `info` so the default `warn` filter excludes them
  until you opt in (`NBOX_LOG=warn,nbox::audit=info`), and they follow the usual
  stderr/`--log-file` sink (never stdout). `--rate-limit <N>` (or
  `[serve].rate_limit`) caps each caller at N requests/minute, keyed on the caller
  (`sub`, else `client_id`, else peer IP) so callers are isolated; over the limit
  → `429` + `Retry-After`, audited as `rate-limited`. Absent / `0` = disabled (the
  default — existing behavior is unchanged unless opted in); the flag wins over
  config. Applies to `/mcp` only, not `/.well-known/*`. Documented as **read-only
  Pattern 3** (DESIGN §24): the audit log attributes calls to the verified caller,
  but the last hop to NetBox still uses the one local service token, so this is
  accountability, not per-user RBAC — suitable for a trusted, read-only,
  single-team deployment. Per-user identity→NetBox-token RBAC (the Pattern 2
  vault) is v2. Behind the `http` cargo feature.
- OIDC resource-server auth for the HTTP transport (`nbox serve`). Setting
  `--oidc-issuer <URL>` + `--audience <VALUE>` (or `[serve].oidc_issuer` /
  `audience`) puts nbox in OAuth 2.1 resource-server mode: inbound IdP JWTs are
  validated on `/mcp` and Protected Resource Metadata (RFC 9728) is advertised at
  `GET /.well-known/oauth-protected-resource` (public, no auth). Provider-agnostic
  (Okta, Entra, Keycloak, Authentik, …). Validation: bearer from the
  `Authorization` header only (query-string tokens rejected); JWT signature via
  the issuer's JWKS selected by `kid` with an explicit RS256/ES256 allowlist (the
  token's `alg` is never trusted, `none` rejected); `iss` exact-match; `aud`
  contains the configured audience (RFC 8707); `exp` with a ≤120 s clock-skew
  leeway. The 8 read tools require the `nbox:read` scope (`nbox:write` is wired for
  future writes). JWKS is cached by `kid` with a single rate-limited, single-flight
  refresh on an unknown `kid`, keeping all published keys (rotation overlap); a
  transient JWKS outage keeps serving from cache (serve-stale), an unknown-`kid`
  cache miss during an outage fails closed. Failures use the standard challenges —
  `401 invalid_token` (+ `resource_metadata`) and `403 insufficient_scope`
  (+ `scope`); the token is never logged or echoed. The JWKS URL is discovered
  from the issuer's `/.well-known/openid-configuration` (then
  `oauth-authorization-server`) unless `--oidc-jwks-url` overrides it. With OIDC
  configured a routable `--http` bind is allowed (the loopback restriction is
  lifted) — terminate TLS in front (reverse proxy); nbox serves plain HTTP and
  warns on a non-loopback bind. Flags win over config, exactly as the loopback
  path. The validated caller identity (`sub`, `client_id`/`azp`, `scope`, `jti`,
  `iss`) is plumbed into request extensions for the upcoming audit log + NetBox
  identity bridge; the last hop to NetBox still uses the local profile token for
  now. Behind the `http` cargo feature (`jsonwebtoken` with the pure-Rust crypto
  backend; JWKS fetch/cache hand-rolled over `reqwest`).
- `nbox serve --http <ADDR>` — loopback HTTP transport for the MCP server, shipped
  in the default build (behind the `http` cargo feature, which is on by default;
  `--no-default-features` for a lean stdio-only build). The same eight read-only
  tools and handler the stdio path serves are mounted at `/mcp` over rmcp's
  Streamable HTTP server (`LocalSessionManager`); stdio stays the zero-config
  default and is unchanged. Loopback only: a non-loopback `<ADDR>` is a usage error (exit `2`) —
  binding a routable interface needs the OIDC auth mode coming later. The `Origin`
  header is validated on every request (non-loopback → 403, DNS-rebinding
  defense), `MCP-Protocol-Version: 2025-11-25` is advertised, and an optional
  static bearer (`--http-token`, or `NBOX_SERVE_TOKEN`, or `[serve].http_token`)
  guards `/mcp` (constant-time compare; missing/wrong → 401). The token is never
  logged; stdout stays clean (the protocol travels over the HTTP body, logs go to
  stderr). Configurable via a new `[serve]` section (`http`, `http_token`); flags
  win over the config. Built without the feature, `--http` errors cleanly.
- `nbox vlan` now surfaces the VLAN group's scope. A VLAN group is itself
  polymorphically scoped (the VLAN is not), so when a VLAN belongs to a scoped
  group the view gains `group_scope` (the group's scope object name) and
  `group_scope_type` (a friendly label). These are additive and distinct from the
  VLAN's own `scope`/`scope_type`; both are omitted when the VLAN has no group or
  the group is unscoped. The extra group fetch happens only when a group exists.
- `nbox search` and prefix lookups gain `--region`, `--site-group`, and
  `--location` scope filters alongside `--site` (NetBox 4.2+ polymorphic scope).
  At most one may be given (more than one is a usage error, exit `2`); each
  resolves the reference to an id and filters by `scope_type` + `scope_id`
  (devices use the `region_id`/`site_group_id`/`location_id` filters). An unknown
  reference errors (exit `4`) rather than returning empty. The same filters are
  exposed as `region`/`site_group`/`location` params on the `nbox_search` MCP tool.
- File logging: a global `--log-file <PATH>` flag (and config `log_file` /
  `log_level`) tees `tracing` output to a file via a non-blocking
  `tracing-appender` writer. Level precedence is flag > config > `NBOX_LOG` >
  `RUST_LOG` > `warn`; the file is flag > config > none. The writer's
  `WorkerGuard` is held for the process lifetime so buffered lines flush on exit.

### Changed
- **Token resolution precedence reversed.** The order is now the profile's
  `token_env` variable (if set & present) → `NBOX_TOKEN` → the OS keyring entry
  for the profile → none. Previously `NBOX_TOKEN` took precedence over the
  profile's `token_env`. Env still always overrides the keyring (CI/SSH/break-glass
  paths set an env var; the keyring is for interactive onboarding). If you relied
  on `NBOX_TOKEN` to override a `token_env` per invocation, unset `token_env` for
  that profile or use `--profile`. `nbox config token status` shows the active
  source so the precedence is visible.
- `nbox man` can now generate the full man-page set, not just the top-level page.
  Bare `nbox man` still streams `nbox.1` to stdout (unchanged — `nbox man >
  nbox.1` keeps working), but `nbox man <dir>` writes the top-level `nbox.1` plus
  one `nbox-<subcommand>.1` per subcommand into that directory, so `man
  nbox-device` resolves once installed. Per-subcommand flags (e.g. the `serve`
  and `search` options) only ever appeared on the per-subcommand pages, which
  nothing emitted before; the release artifact now packages the whole set under
  `completions/man/` in `nbox-completions.tar.gz`.
- CI now gates the lean build: `--no-default-features` clippy/build/test run
  alongside the existing `--all-features` steps, so the no-default-features path
  can't silently regress.
- Docs reconciled with this session's features and hardening: ROADMAP ticks
  (virtualization/tenancy detail views, TUI profile switcher, MCP resources;
  VRF-aware navigation marked in-progress), README usage/search/MCP coverage of
  the new `provider`/`tenant`/`contact`/`vm`/`cluster` lookups and resources,
  `KNOWN_ISSUES` updated for the now-shipped `search --vrf`/scope filters, and the
  `[lints]`-table pedantic gate noted in CONTRIBUTING/RELEASING. Docs-only.
- Internal: the `non_empty` (drop empty string → `None`) and `non_zero` (drop
  zero count → `None`) filters the detail views all duplicated are now shared
  `pub(crate)` helpers in `src/domain/util.rs`, replacing ~17 local `non_empty`
  closures and 3 local `non_zero` fns. Pure refactor — output is byte-identical.
- `clippy::pedantic` is now enforced across all crates incl. tests via a
  `[lints]` table. The pedantic gate + curated allow-list moved from the
  `src/lib.rs` / `src/main.rs` inner attributes into `[lints.clippy]` in
  `Cargo.toml`, so it covers the lib, bin, AND the integration test crates in
  `tests/` uniformly (inner attributes reached only the lib/bin). The standing
  `cargo clippy --all-targets --all-features -- -D warnings` CI step is now a
  true whole-project pedantic gate.
- The TUI help is now a centered modal overlay drawn over the live screen
  (ttl/xfr style), replacing the old full-screen Help screen. `?`/`F1` toggle it;
  any key or `Esc` closes it (consumed — no underlying action fires). The `cheese`
  Help wrapper was dropped; the layout helpers are pure and unit-tested.
- TUI scroll/position indicators in pane titles. The results list now shows a
  `selected/len` row counter in its title corner (e.g. ` 3/47 `), and the detail
  and preview panes show a scroll-position percentage (e.g. ` 50% `) whenever
  their body overflows the pane — so a long view reads as scrollable rather than
  silently clipped. The indicators only appear when there's something to scroll
  (a list with rows / a body taller than the pane) and are dimmed via the theme's
  `text_dim`. No keybindings changed; the hint helpers are pure and unit-tested.

### Fixed
- TUI: a rapid profile re-switch could settle a stale reconnect. Switch
  completions were correlated by profile *name*, so a sequence like alpha → beta
  → gamma → beta again let the OLDER beta's reconnect settle the NEWER beta
  attempt (same name, so the "is this still current?" check passed for the wrong
  one) — leaving the client/header reflecting a stale instance. Each initiated
  switch now carries a monotonic switch id (the same idea as the search/detail
  per-channel request-id guard); a `ProfileSwitched` whose id is older than the
  latest initiated switch is dropped on arrival — even one to the same profile
  name. The name is kept for display, but correctness rides the id. Composes
  with the existing switch hardening: the deferred header flip, fetch fencing
  while a switch is pending, no phantom header on failure, and the
  header-always-matches-connected-client invariant all hold, and a dropped
  superseded completion can't clear a newer switch's pending state.
- The `scripts/install.sh` quick-install script could not install a real release.
  It mapped Linux hosts to `*-unknown-linux-gnu` triples, but `release.yml` only
  ships static **musl** archives for Linux x86_64/aarch64 — so the download 404'd
  (and even when a target did exist, the install step looked for the binary under
  a `nbox-<target>/` subdir that the bare-binary tarball never contains). The
  script now maps Linux x86_64→`x86_64-unknown-linux-musl` and
  aarch64/arm64→`aarch64-unknown-linux-musl` (macOS unchanged), and locates the
  extracted `nbox` by search rather than a hardcoded path, matching the actual
  tarball layout (and what the Homebrew formula's `bin.install "nbox"` expects).
  Unsupported hosts still fall back to `cargo install nbox`.
- `--no-tui` is now honored. The flag was defined and documented but ignored in
  dispatch, so a bare `nbox --no-tui` still launched the interactive TUI — bad for
  agents/scripts that pass it to guarantee non-interactive behavior. Any invocation
  that would otherwise launch the TUI (a bare `nbox`, or an explicit `nbox tui`) now
  refuses with a usage error (exit `2`) and an explanation on stderr, leaving stdout
  clean; `nbox tui` is refused too (a script that sets `--no-tui` never gets a
  terminal UI, whatever follows). `--no-tui` is a no-op on any other subcommand,
  which never launches the TUI anyway.
- TUI command palette `ip <address>` lookups now route through the same
  ambiguity-aware resolver the CLI/MCP use. The palette path took the first of
  `ip_candidates()`, so an address present in more than one VRF would silently
  open the wrong object. An ambiguous (or not-found) reference now surfaces as an
  error status instead, leaving the home screen in place; the unambiguous case is
  unchanged.
- TUI: a slow earlier search or detail load could land after a newer one and
  clobber the screen (untagged `SearchComplete`/`DetailLoaded` events). Each
  spawned full search/detail request now carries a monotonic per-channel request
  id (the same idea as the preview pane's `(kind, id)` tag); a result whose id is
  older than the latest spawned for its channel is dropped on arrival, so only the
  newest applies. Navigation, manual/auto refresh, and the recents path all ride
  the guard.
- `nbox prefix <cidr> --vrf <ref>` now scopes its child-prefix and contained-IP
  sections to the resolved prefix's VRF. The prefix itself was VRF-aware, but its
  children (`within`) and member IPs (`parent`) were filtered by CIDR only, so a
  CIDR that exists in more than one VRF could show another VRF's children/IPs.
  `prefix_children`/`prefix_ips` now take a `vrf_id` (the prefix's VRF, or `null`
  for the global table) — mirroring the VRF-scoped `prefixes_containing` used by
  `nbox ip` — and the CLI, MCP, and TUI prefix-detail paths all pass it through.
- Scope disambiguation now prefers an exact match. `--site`/`--vrf`/`--group`
  matched the scope's `display` by substring, so `nbox vlan 1234 --site ci-site`
  also matched a prefix sibling like `ci-site2` (whose display contains
  `ci-site`) and stayed ambiguous instead of resolving. `retain_scope` now keeps
  candidates whose scope matches the reference exactly (name/slug/id) when any
  do, and only falls back to the loose substring match when none do — so
  `--vrf <rd>` still resolves.
- `--vrf <rd>` now resolves a VRF by route distinguisher *exactly*, via a real
  field. The `BriefObject` brief gained an `rd` field (NetBox's VRF serializer
  includes it), so `BriefObject::matches`/`matches_exact` compare the RD against
  the dedicated `rd` rather than substring-matching the `display` (e.g.
  `blue (65000:1)`) — the old path only worked by accident and could match a
  display that merely contained the string. `--vrf 65000:1` now matches the RD
  exactly; a non-matching RD no longer slips through, and `matches_exact` stays
  strict (name/slug/id/rd, never a display substring).
- `nbox search --region/--site-group/--location <ref>` now accepts a numeric id,
  not just a slug/name. The clap/CLI help promised ids, but `region_by_ref`/
  `site_group_by_ref`/`location_by_ref` (and `site_by_ref`) resolved by slug/name
  only, so `--region 5` fell through to a name lookup. Each now tries the by-id
  detail endpoint first (404 → unresolved), mirroring `device_by_ref`/`vrf_by_ref`.
- `nbox search --region/--site-group/--location <ref>` now also includes scoped
  clusters. Clusters carry NetBox 4.2+'s polymorphic `scope` (the same as
  prefixes), but cluster search was skipped for the id-based scope filters. It now
  filters by `scope_type=dcim.region|dcim.sitegroup|dcim.location` + `scope_id`,
  the same way prefixes do (and `--site` flows through the same scope path).
- `nbox serve --http` (OIDC/routable mode, `http` feature): a real proxied request
  with the deployment's `Host` (e.g. `nbox.example.com`) was `403`'d because rmcp's
  Streamable HTTP server kept its loopback-only `Host` allow-list even when a
  routable bind was permitted. The allow-list is now widened in OIDC mode to the
  `--audience` host (nbox's own identity) plus loopback, with `--allowed-host`
  (repeatable) / `[serve].allowed_hosts` to add more; loopback mode keeps the
  strict loopback-only default.
- `nbox serve --http`: the `MCP-Protocol-Version` response header was missing from
  the `401`/`403` auth-challenge and `429` rate-limit responses (it was only added
  on the success path). Every response from the `/mcp` gate now carries it.
- TUI: the profile switcher (`P` / `Ctrl+P`, palette `profile <name>`) could query
  the wrong instance and leave a phantom header. The header flipped to the target
  profile/URL *before* the reconnect finished while searches/details still hit the
  old client, and on a failed reconnect the header was left pointing at the
  unreachable profile while the client stayed on the old one — the UI claimed a
  server it wasn't talking to. The header now flips only when the switch
  **succeeds** (the client swap, header/URL/version update, stale-data clear and
  request-generation bump all apply atomically), new search/detail/preview fetches
  are **fenced** while a switch is in flight (so the old client is never queried
  mid-switch), and a failed switch is a no-op + error toast that keeps the previous
  profile + client. The header now always matches the instance the client is
  connected to — in pending, success, and failure.
- TUI: the command palette `:theme <name>` bypassed the `NO_COLOR` guard that the
  `t` theme-cycle already honored, so it could re-enable color under `NO_COLOR` and
  then persist a colored theme on exit. Both theme paths now share one guard, so
  `:theme` respects `NO_COLOR` consistently and no colored theme is written back.
- `nbox search --site <name|id>` now actually filters devices, VLANs, and VMs.
  Those branches resolved the `--site` reference to an id but still passed the
  *raw* user value as `site=<value>`; NetBox's `site` query param wants a slug, so
  a numeric id or display-name `--site` silently matched nothing on those object
  kinds (prefixes/clusters, on the polymorphic `scope`, were unaffected). They now
  filter by the resolved `site_id=<id>`. Devices additionally honor `--region`/
  `--site-group`/`--location` via the resolved `region_id`/`site_group_id`/
  `location_id` (no raw values), and `site` is no longer carried through the
  plain-value allowlist at all — every scope kind goes through its resolved id.
- The numeric resolvers `site_by_ref`/`region_by_ref`/`site_group_by_ref`/
  `location_by_ref`/`vrf_by_ref` no longer dead-end on a 404. A numeric reference
  took a by-id fast-path that returned immediately, *including* returning "not
  found" on a 404, so an object whose slug/name (or VRF RD) is itself numeric (a
  site slug `"5"`) could never resolve once the id lookup missed. The by-id 404
  case now FALLS THROUGH to the slug/name (and RD for VRF) lookups; a genuine id
  hit still short-circuits.
- Install-quality subcommand man pages. `nbox man <dir>` rendered each
  subcommand page from the bare subcommand `Command`, so `nbox-device.1` was
  titled `device(1)` and its SYNOPSIS read `device …` rather than `nbox
  device …`; the `nbox-config.1`/`nbox-profile.1` pages also cross-referenced
  `config-init(1)`/`profile-add(1)` pages that were never generated (dangling
  refs). Each page is now titled for its dashed lookup name (`nbox-device`,
  `nbox-config-init`) while its SYNOPSIS shows the real space-separated
  invocation (`nbox device …`, `nbox config init …`), and the nested
  `config`/`profile` subcommands get their own pages
  (`nbox-config-init.1`, `nbox-profile-add.1`, …) so no cross-reference dangles.
- `nbox search --help` (and the clap-derived `nbox-search.1`) listed `racks`,
  which search has never covered, and omitted the kinds it does — now the
  accurate set: devices, sites, IPs, prefixes, VLANs, circuits, aggregates,
  ASNs, IP ranges, tenants, contacts, providers, VMs, and clusters.
- TUI: onboarding and Config-modal form fields no longer render a stray `>` with
  the cursor two cells off. The cheese `Input` adapter for multi-field forms left
  the widget's default `>` prompt in place; form rows now use an empty prompt, and
  the focused cursor is placed by display-column width (wide glyphs included).
- TUI: renaming a profile in the Config modal no longer leaves an orphaned
  `[profiles.<old>]` section in `config.toml` (a phantom that returned on the next
  launch). A rename now removes the old section and, if it was the active profile,
  repoints `active_profile` to the new name. The OS-keyring entry is migrated to
  the new key too (or cleared/stored per the token field), so a renamed
  keyring-backed profile keeps its auth. When the active profile is renamed with a
  plain save, the running TUI label and active index follow the new name too.
- TUI: a typed token is no longer silently discarded when the OS keyring is
  unavailable. The save paths used to overwrite or hide the keyring warning with
  "saved", "switching", or "switched" statuses, so the user saw success while
  nothing was stored. The warning now survives plain save and successful save+use
  reconnects — it states the token was NOT stored and how to provide one (a
  `token_env` or `NBOX_TOKEN`).
- TUI: editing a probe-relevant field (url / token / `token_env` / auth /
  verify-tls) while a test-connect is in flight no longer shows the old result as
  if it matched the new form. The in-flight probe is superseded (result cleared +
  test id bumped) in both onboarding and the Config-modal editor.
- TUI: test-connect now builds its probe token with the same precedence as a real
  save/launch — typed token → form `token_env` → `NBOX_TOKEN` → keyring — so
  changing `token_env` actually tests the new source (it previously tested the old
  typed-only / saved-profile token).
- TUI: the Config-modal save+use action moved from `Ctrl+U` (which collided with
  the text field's clear-line) to `Ctrl+G`; `Ctrl+U` now clears the focused field.
  The edit form gained `Ctrl+X` to clear a stored keyring token on save.
- TUI: adding a profile whose name already exists is rejected on save (a rename
  onto another existing profile is likewise blocked); cancelling an add/edit with
  `Esc` returns to the previously selected list row instead of snapping to the top.
- TUI/config: a profile save with no backing config-file path now surfaces an
  error instead of a misleading "saved".
- config: an empty OS-keyring entry is treated as "no token" rather than an empty
  string (which produced a confusing `401` instead of a clean "no token").
- `--no-tui` now also refuses the first-run onboarding wizard (exit `2` with setup
  guidance), matching its refusal of the interactive TUI.
- TUI/config: the Settings save batches all changed `[ui]` fields into a single
  format-preserving write, so a mid-save failure can't leave the file with one
  field updated and the rest stale.
- TUI: a bare cursor move (Left/Right/Home/End) in a text input no longer counts
  as an edit, so it doesn't needlessly refilter a search or invalidate a
  test-connect result.
- TUI: the right-pane preview body is fetched once per frame and borrows the
  loaded detail instead of cloning the whole string twice; the Config-modal key
  path no longer clones every profile name per keystroke.
- TUI: the test-connect keyring lookup runs inside the spawned probe task instead
  of on the render/event thread, so the UI never blocks on the keychain.
- docs: `docs/CONFIG.md` and `examples/config.toml` now document the `[serve]`
  section (http / http_token / oidc_issuer / audience / jwks_url / allowed_hosts /
  rate_limit), noting `http_token` is a secret (prefer the env var).
- `nbox open` / the TUI browser-open now treat a non-zero exit from a custom
  `open_browser_command` as an error instead of reporting success.
- The unused `[ui].wide` knob (nothing read it) is no longer written by
  `config init` / the example config, nor exposed as a field; an existing
  `wide = …` in a user's file is harmlessly ignored.
- A pasted token is trimmed of surrounding whitespace before it's stored, so a
  trailing newline from a paste no longer breaks auth.
- `keyring_get` now distinguishes a missing entry (the silent "no token" case)
  from a real backend failure, which is logged at debug while still returning
  `None` for the UI.
- The no-echo `config token set` reader handles the Delete key (like Backspace)
  instead of ignoring it.
- The first-run wizard shows the "set NBOX_TOKEN / a token_env" guidance on its
  own final frame when no token landed, not only after it exits.
- The Config modal and onboarding wizard show a compact "terminal too small" hint
  on a tiny terminal instead of a collapsed, garbled layout.
- docs: `docs/MCP.md` and a `tests/it_netbox.rs` comment now state the real token
  precedence (token_env → `NBOX_TOKEN` → keyring) after the earlier reversal.

### Security
- `nbox config show` no longer prints `serve.http_token` — the one secret that can
  live in `config.toml`. It is redacted to `<redacted>` in both the human TOML and
  the `--json` output (an absent token stays absent, so you can still tell whether
  one is configured without revealing it).
- `ServeConfig` has a hand-written `Debug` that redacts `http_token`, so a `{:?}`
  or log line of a `Config` can never leak the serve token.
- The OS-keyring account key is now collision-safe: it length-prefixes the config
  path so the path/profile boundary is unambiguous (a `{path}::{profile}` join
  could otherwise alias two different (path, profile) pairs onto one secret).
- `keyring_set` rejects an empty token (it would otherwise round-trip as a no-op),
  and the `TokenAction` carrying a typed token redacts its value in `Debug`.
- `nbox serve --http` (OIDC mode, `http` feature): the HTTPS-only rule for the IdP
  issuer / JWKS / discovered endpoints is now enforced on **every HTTP redirect
  hop**, not just the original URL. The IdP client previously followed redirects
  with reqwest's default policy, so an `https://` endpoint could `30x`-redirect the
  discovery/JWKS fetch down to a plain-`http://` non-loopback URL and silently
  downgrade the transport the validation was meant to guarantee. A custom redirect
  policy now re-checks `https-or-loopback` on each hop's target and fails the
  request on any non-HTTPS/non-loopback hop (a loopback http hop is still allowed
  for local dev); the chain is also capped. The original-URL checks remain (defense
  in depth).
- `nbox serve --http`: a flood of **unauthenticated / invalid-bearer** requests
  from one peer is now rate-limited. The auth check returned `401`/`403` before the
  rate limiter, so missing/invalid-token requests were never throttled and could
  hammer JWT validation. `--rate-limit` now also applies a coarse per-peer-IP cap
  *before* authentication; authenticated requests still honor their per-caller
  (`sub`/`client_id`) cap as well. The pre-auth `429` carries `Retry-After` and the
  `MCP-Protocol-Version` header and is audited (attributed to the peer IP, no
  identity). `--rate-limit 0` / absent disables both levels (unchanged default).
- `nbox serve --http` (OIDC mode, `http` feature): an `--allowed-host` /
  `[serve].allowed_hosts` entry — or the `--audience` host — with a **malformed
  port** is now rejected at startup (`exit 2`, naming the entry) instead of failing
  open. The port-aware parser previously dropped a present-but-invalid port (out of
  range like `nbox.example.com:99999`, non-numeric like `nbox.example.com:abc`, or
  empty after the `:`), leaving a bare host that matched on **any** port — the
  opposite of an operator who typed an explicit port intended, *broadening* the
  allow-list. A port component must now parse as a valid `1`–`65535`; IPv6 literals
  are handled correctly (`[::1]` is port-less, `[::1]:8443` is valid, `[::1]:99999`
  is rejected — the colons inside the brackets are not a port separator). A
  genuinely port-less entry keeps its any-port behavior, and an inbound request
  `Origin` with a malformed port fails closed (`403`).
- `nbox serve --http` (OIDC mode): an `--allowed-host` (or `--audience` host) entry
  with an **explicit port** now matches only that `host:port` for the DNS-rebinding
  `Host`/`Origin` checks. Normalization previously stripped the port, so
  `nbox.example.com:8443` was reduced to `nbox.example.com` and matched the host on
  any port — broadening the allow-list beyond what the operator specified. A
  port-less entry keeps host-only (any-port) matching; loopback still passes on any
  port; the `Host` and `Origin` checks apply the rule identically.
- `nbox serve --http` (OIDC mode, `http` feature): the IdP issuer, the JWKS URL
  (override or discovered), and any discovered endpoint must now use `https://`
  unless the host is loopback (local dev). A plain-`http://` non-loopback IdP URL
  is rejected at startup (`exit 2`) instead of nbox fetching signing keys over
  plaintext — closing a key-substitution vector.
- `nbox serve --http`: `Origin` validation now runs in **both** loopback and OIDC
  modes against the same allowed-host set used for the `Host` check (a real
  DNS-rebinding defense in routable mode, not just loopback). The docs previously
  claimed Origin was validated on every request while the code only enforced it in
  loopback mode; code and docs are now consistent.
- `nbox serve --http`: the raw `Mcp-Session-Id` is no longer written to the audit
  log. The audit event now records `session` — a short SHA-256 prefix of the
  session id — which stays correlatable across a session's requests without
  putting the raw session handle in the log.
- The `cargo audit` release gate stays strict, with one documented exception in
  `.cargo/audit.toml`: RUSTSEC-2023-0071 (the `rsa` "Marvin Attack" timing
  side-channel, no fix available). It reaches us only via `jsonwebtoken`'s
  `rust_crypto` backend, used solely for OIDC JWT signature *verification* (a
  public-key operation); the binary performs no RSA private-key operations, so the
  attack does not apply. See the file for the full rationale.

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
- The dependency manifest keeps `rmcp`, `update-informer`, and `rusqlite` in the cross-platform `[dependencies]` table. A `[target.'cfg(unix)'.dependencies]` block (added for `libc`) had been placed mid-list, which silently scoped every dependency below it to unix-only and broke the Windows release build (`cannot find crate rmcp`). Only `libc` is unix-gated now.

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

[0.3.0]: https://github.com/lance0/nbox/releases/tag/v0.3.0
[0.2.0]: https://github.com/lance0/nbox/releases/tag/v0.2.0
[0.1.1]: https://github.com/lance0/nbox/releases/tag/v0.1.1
