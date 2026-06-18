# Changelog

All notable changes to nbox are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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

### Fixed
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

### Security
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

[0.1.1]: https://github.com/lance0/nbox/releases/tag/v0.1.1
