# Features

nbox is a read-only NetBox client — a CLI and a TUI over the same core.

## Lookups

| Command | What |
| ------- | ---- |
| `nbox search <q>` | Parallel search across devices/sites/racks/IPs/prefixes/VLANs/circuits/virtual-circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers/VMs/clusters/VRFs/route-targets. Filters: `--status/--site/--region/--site-group/--location/--tenant/--role/--tag/--vrf`, `--limit`, `--cols`, `--partial`. |
| `nbox device <name\|slug\|id> [--journal]` | Device + interfaces, IPs, cables, VLANs, services. |
| `nbox interface <device> <iface>` | One interface: type, MTU, MAC, mode, VLANs, cable, **cable path** (an A↔Z trace diagram naming the device at each end), addresses. |
| `nbox ip <addr> [--vrf] [--journal]` | IP + most-specific parent prefix (VRF-scoped) and its VLAN plus the prefix's `scope`/`scope_type` (site, location, region, …); surfaces `nat_inside`/`nat_outside` (NetBox 4.6) when set. |
| `nbox prefix <cidr> [--vrf] [--journal]` | Prefix with utilization, children, and contained IPs. |
| `nbox next-ip <cidr> [--count] [--vrf]` | Next available address(es). |
| `nbox next-prefix <cidr> [--length] [--vrf]` | Available free block(s). |
| `nbox vlan <vid\|name> [--site] [--group] [--journal]` | VLAN + referencing prefixes, plus the VLAN's own `scope`/`scope_type` and, when it belongs to a scoped VLAN group, the group's `group_scope`/`group_scope_type`. |
| `nbox site` / `rack` / `circuit` / `virtual-circuit` / `aggregate` / `asn` / `ip-range` `[--journal]` | Object lookups. |
| `nbox tenant <slug\|name\|id>` | Tenant: group, description, relation counts, tags, custom fields. |
| `nbox contact <name\|id>` | Contact: title, phone, email, address, link, group, tags, custom fields. |
| `nbox provider <slug\|name\|id>` | Provider: ASNs, accounts, description, circuit count, tags, custom fields. |
| `nbox vm <name\|id>` | Virtual machine: status, role, cluster, device, platform, vcpus, memory, disk, primary IPs, tenant, site, description, tags, custom fields. |
| `nbox cluster <name\|id>` | Cluster: type, group, status, tenant, scope (site/region/…), device + VM counts, description, tags, custom fields. |
| `nbox vrf <name\|rd\|id>` | VRF as a routing context: summary (RD, tenant, enforce-unique, import/export route targets, counts) plus its prefix tree and scoped addresses. |
| `nbox route-target <name\|id>` | Route target (e.g. 65000:100): tenant/description plus the VRFs that import and export it (navigable). |
| `nbox mac <addr>` | Reverse-resolve a MAC to the interface(s)/device(s) that carry it (NetBox 4.2+). Any common form is normalized (`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF`, `aa-bb-…`, `aabbccddeeff`); a non-MAC is a usage error, several carrying interfaces are ambiguous. |
| `nbox tags` | List tags. |
| `nbox tagged <tag>` | Objects carrying a tag, across kinds (NetBox 4.3+ `/api/extras/tagged-objects/`); tag = id/name/slug. |
| `nbox journal <kind> <ref>` | Recent journal entries for an object. Kinds: device, ip, prefix, vlan, site, rack, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster, vrf, route-target, interface (`<device>/<name>`). `--journal` on a detail lookup folds the most recent entries inline (default 5); `--journal-limit <N>` overrides the cap and implies `--journal`. (`tenant`/`contact`/`provider`/`vm`/`cluster`/`vrf`/`route-target`/`interface`/`virtual-circuit` have no inline `--journal` flag — use `nbox journal`.) |
| `nbox status` | Connection + per-surface `api` routing (configured/effective) + capabilities + NetBox/Django/Python versions + a token-validity preflight (`token`: `valid`/`invalid`/`unverified`; NetBox 4.5+). |
| `nbox open <kind>/<ref>` | Open an object in the browser. Kinds: device, ip, prefix, vlan, site, rack, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, cluster, vrf, route-target, mac, and `interface/<device>/<name>` (the interface name may contain slashes, e.g. `xe-0/0/1`). |
| `nbox raw GET <path>` | Raw read-only API request (escape hatch). |

Every detail lookup surfaces the object's `tags` (joined slugs in plain output, a
`tags` array in `--json`), dropped when the object has none, plus its non-empty
custom fields as `cf.<name>`.

Duplicate references across scopes (an address/CIDR in several VRFs, a VID at
several sites) exit `5` and list the candidates; scope with `--vrf`/`--site`/`--group`.

`search --site/--region/--site-group/--location <ref>` resolves the reference
once (by slug, name, or **id**) to a numeric id and filters prefixes by that
scope — NetBox 4.2 replaced the prefix `site` field with the polymorphic `scope`,
so prefixes are matched on `scope_type=dcim.site`/`dcim.region`/`dcim.sitegroup`/
`dcim.location` + `scope_id`, not the dead `?site=` filter. The match is
**exact**: each flag filters by its own scope only (no hierarchy/descendant
expansion — `--region` does not pull in prefixes scoped to sites inside that
region). At most **one** scope flag may be set (the prefix `scope` is a single
type+id); passing more than one is a usage error (exit `2`). An unknown reference
is a not-found error (exit `4`), not a silent empty result. Non-prefix endpoints
filter by the **resolved id**, never a raw value (the plain `?site=` param wants a
slug, so a `--site` given as an id or display name would silently match nothing):
clusters carry the same polymorphic scope, so they honor all four scopes via
`scope_type`+`scope_id`; devices honor every scope via `site_id`/`region_id`/
`site_group_id`/`location_id`; VLANs and VMs honor `--site` via `site_id`;
endpoints that can't filter by a given scope are skipped rather than sent a dead
param.

`search --vrf <id|rd|name>` resolves the VRF once (numeric id, then RD, then
name — VRFs have no slug) and filters the VRF-capable endpoints (IPs, prefixes)
by `vrf_id=`. Endpoints that carry no VRF (devices, sites, VLANs, circuits, …)
are skipped for this filter (queried unfiltered, not dropped). `--vrf` is
orthogonal to the scope filters above — both may be set, and NetBox ANDs them on
prefixes. An unknown VRF is a not-found error (exit `4`), not a silent empty
result.

Profiles opt the **VRF view** into NetBox GraphQL under `[profiles.<name>.api]` —
`vrf` (the VRF view's prefix/address bundle) is `rest` (default) or `graphql`. A
GraphQL surface returns the same normalized shape and keeps the same fail-closed/
`--partial` behavior, and **falls back to REST** (with the reason shown by `nbox
status`) when the live schema can't support it. nbox probes `/graphql/` at
runtime and adapts to the schema: NetBox 4.2's unpaginated list fields, NetBox
4.3+'s offset pagination, and NetBox 4.5+'s lookup-wrapper filters are all shaped
from introspection rather than version strings. **Search is always REST** —
NetBox's GraphQL API has no equivalent to REST's full-text `q`, so a `search =
"graphql"` preference transparently falls back. REST stays canonical and powers
search, identity resolution, detail lookups, raw reads, journals, and
available-IP/prefix commands. (The old coarse `backend = …` profile key was
removed.)

## Output

Every data command takes `-o plain|json` (`--json` is shorthand). JSON adds
`--fields a,b,c`, `--raw` (compact), and `--envelope` (`{schema_version, data}`).
`-o csv` is for tabular/list results (e.g. `search`); single objects are rejected
(use `--json` or plain). stdout stays clean for piping; logs/errors go to stderr.
See [AGENTS.md](../AGENTS.md) for the machine-readable surface and exit codes.

## TUI

`nbox` (no subcommand) launches the TUI — a three-pane home (a navigation rail of
browsable kinds → results → a live detail preview):

- `/` search — or, on a browse kind, a server-side filter on that list: a name
  substring (`name__ic`) for devices/sites/racks/VLANs/VRFs/route-targets, or
  network containment for prefix (`within_include`) and IP (`parent`) browse
  (type a CIDR, Enter; the title reads `within "10.0.0.0/24"`). `:` command palette,
  `f`/`F` filter / clear, `Tab`/`Shift+Tab` move between panes (or cycle detail
  tabs), `j`/`k` move (live-browse the kind while on the nav rail), `g`/`G`
  top/bottom, `Enter` open.
- `o` open in browser, `y` copy, `R` related objects (jump between connected
  objects), navigable device tabs `i`/`p`/`c`/`v`/`s` (`j`/`k` + `Enter` opens a
  row — interfaces/cables open the interface detail, which has a cable-path A↔Z
  diagram), `e` rack elevation.
- `D` overview dashboard, `T` prefix tree (`Space`/`←`/`→` collapse/expand), `t`
  cycle theme, `r` refresh, recents on the home screen, optional auto-refresh
  (`[ui].refresh_secs`).
- `P`/`Ctrl+P` switch profile live; `S` opens the Config modal — add/edit/select
  profiles and a settings editor (appearance, behavior, logging). First run with
  no config drops into a guided onboarding wizard instead.

Twelve themes (`NO_COLOR` honored); `?`/`F1` shows the full keymap.

## MCP server

`nbox serve` is a read-only MCP server over stdio. An MCP host launches it as a
subprocess and speaks JSON-RPC over stdin/stdout; the tools reuse the CLI's query
+ view layer and return the same JSON view models. JSON-RPC on stdout, logs on
stderr; URL/token from the active profile (same `-p`/`--config` flags). All tools
are annotated read-only.

| Tool | What |
| ---- | ---- |
| `nbox_status` | Connection + active backend capabilities + NetBox/Django/Python versions + a token-validity preflight (`token`: `valid`/`invalid`/`unverified`; NetBox 4.5+). |
| `nbox_search` | Search devices/sites/racks/IPs/prefixes/VLANs/circuits/virtual-circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers/VMs/clusters/VRFs/route-targets; `query`, `limit`, `status`, `site`, `region`, `site_group`, `location`, `tenant`, `role`, `tag`, `vrf` (id\|rd\|name; IP/prefix only). |
| `nbox_get` | One object by `kind` (device, ip, prefix, vlan, site, rack, circuit, virtual_circuit, aggregate, asn, ip_range, tenant, contact, provider, vm, cluster, vrf, route_target, mac, interface) + `ref`; `vrf`/`site`/`group` disambiguate. |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix. |
| `nbox_next_prefix` | Available child prefix(es) of a given length, or all free blocks. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_list_tags` | List tags. |
| `nbox_tagged` | Objects carrying a tag, across kinds (NetBox 4.3+); `tag` (id\|name\|slug). Cross-kind reverse lookup. |
| `nbox_cache_clear` | Drop nbox's local read cache so the next lookups fetch fresh (read-only w.r.t. NetBox). |

A loopback HTTP transport ships in the default build (behind the `http` cargo
feature, on by default; `--no-default-features` for stdio-only):
`nbox serve --http 127.0.0.1:8080`, optional static bearer — same tools mounted at
`/mcp`, loopback only with `Origin`/`Host` validation. Add `--oidc-issuer` +
`--audience` for OAuth 2.1 resource-server mode (inbound IdP JWT validation,
Protected Resource Metadata, routable bind allowed) — accountability, not per-user
RBAC. See [docs/MCP.md](MCP.md). The same objects are also exposed as MCP
**resources** via one `nbox://{kind}/{ref}` template (the view `nbox_get`
returns). Per-user NetBox identity bridging, a raw escape-hatch tool, and MCP
prompts are later.

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.
