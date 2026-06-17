# Features

nbox is a read-only NetBox client (v0.1) — a CLI and a TUI over the same core.

## Lookups

| Command | What |
| ------- | ---- |
| `nbox search <q>` | Parallel search across devices/sites/IPs/prefixes/VLANs/circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers. Filters: `--status/--site/--region/--site-group/--location/--tenant/--role/--tag/--vrf`, `--limit`, `--cols`, `--partial`. |
| `nbox device <name\|slug\|id> [--journal]` | Device + interfaces, IPs, cables, VLANs, services. |
| `nbox interface <device> <iface>` | One interface: type, MTU, MAC, mode, VLANs, cable, **cable path** (trace), addresses. |
| `nbox ip <addr> [--vrf] [--journal]` | IP + most-specific parent prefix (VRF-scoped) and its VLAN plus the prefix's `scope`/`scope_type` (site, location, region, …). |
| `nbox prefix <cidr> [--vrf] [--journal]` | Prefix with utilization, children, and contained IPs. |
| `nbox next-ip <cidr> [--count] [--vrf]` | Next available address(es). |
| `nbox next-prefix <cidr> [--length] [--vrf]` | Available free block(s). |
| `nbox vlan <vid\|name> [--site] [--group] [--journal]` | VLAN + referencing prefixes, plus the VLAN's own `scope`/`scope_type` and, when it belongs to a scoped VLAN group, the group's `group_scope`/`group_scope_type`. |
| `nbox site` / `rack` / `circuit` / `aggregate` / `asn` / `ip-range` `[--journal]` | Object lookups. |
| `nbox tenant <slug\|name\|id>` | Tenant: group, description, relation counts, tags, custom fields. |
| `nbox contact <name\|id>` | Contact: title, phone, email, address, link, group, tags, custom fields. |
| `nbox provider <slug\|name\|id>` | Provider: ASNs, accounts, description, circuit count, tags, custom fields. |
| `nbox tags` | List tags. |
| `nbox journal <kind> <ref>` | Recent journal entries for an object. Kinds: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider. `--journal` on a detail lookup folds the most recent entries inline (default 5); `--journal-limit <N>` overrides the cap and implies `--journal`. (`tenant`/`contact`/`provider` have no inline `--journal` flag — use `nbox journal`.) |
| `nbox status` | Connection + NetBox/Django/Python versions. |
| `nbox open <kind>/<ref>` | Open an object in the browser. Kinds: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range, tenant, contact, provider, and `interface/<device>/<name>` (the interface name may contain slashes, e.g. `xe-0/0/1`). |
| `nbox raw GET <path>` | Raw read-only API request (escape hatch). |

Duplicate references across scopes (an address/CIDR in several VRFs, a VID at
several sites) exit `5` and list the candidates; scope with `--vrf`/`--site`/`--group`.

`search --site/--region/--site-group/--location <ref>` resolves the reference
once (by slug, name, or id) and filters prefixes by that scope — NetBox 4.2
replaced the prefix `site` field with the polymorphic `scope`, so prefixes are
matched on `scope_type=dcim.site`/`dcim.region`/`dcim.sitegroup`/`dcim.location`
+ `scope_id`, not the dead `?site=` filter. The match is **exact**: each flag
filters by its own scope only (no hierarchy/descendant expansion — `--region`
does not pull in prefixes scoped to sites inside that region). At most **one**
scope flag may be set (the prefix `scope` is a single type+id); passing more than
one is a usage error (exit `2`). An unknown reference is a not-found error (exit
`4`), not a silent empty result. Non-prefix endpoints: devices honor `--site`
(slug) and the id-based scopes via `region_id`/`site_group_id`/`location_id`;
VLANs honor `--site` directly; endpoints that can't filter by a given scope are
skipped rather than sent a dead param.

`search --vrf <id|rd|name>` resolves the VRF once (numeric id, then RD, then
name — VRFs have no slug) and filters the VRF-capable endpoints (IPs, prefixes)
by `vrf_id=`. Endpoints that carry no VRF (devices, sites, VLANs, circuits, …)
are skipped for this filter (queried unfiltered, not dropped). `--vrf` is
orthogonal to the scope filters above — both may be set, and NetBox ANDs them on
prefixes. An unknown VRF is a not-found error (exit `4`), not a silent empty
result.

## Output

Every data command takes `-o plain|json` (`--json` is shorthand). JSON adds
`--fields a,b,c`, `--raw` (compact), and `--envelope` (`{schema_version, data}`).
`-o csv` is for tabular/list results (e.g. `search`); single objects are rejected
(use `--json` or plain). stdout stays clean for piping; logs/errors go to stderr.
See [AGENTS.md](../AGENTS.md) for the machine-readable surface and exit codes.

## TUI

`nbox` (no subcommand) launches the TUI: `/` search, `:` palette, `Tab` switch
pane, `Enter` open, `o` browser, `y` copy, `t` theme, `r` refresh, device tabs
`i`/`p`/`c`/`v`/`s`, recents on the home screen, optional auto-refresh
(`[ui].refresh_secs`).

## MCP server

`nbox serve` is a read-only MCP server over stdio. An MCP host launches it as a
subprocess and speaks JSON-RPC over stdin/stdout; the tools reuse the CLI's query
+ view layer and return the same JSON view models. JSON-RPC on stdout, logs on
stderr; URL/token from the active profile (same `-p`/`--config` flags). All tools
are annotated read-only.

| Tool | What |
| ---- | ---- |
| `nbox_status` | Connection + NetBox/Django/Python versions. |
| `nbox_search` | Search devices/IPs/prefixes/VLANs/sites/circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers; `query`, `limit`, `status`, `site`, `tenant`, `role`, `tag`, `vrf` (id\|rd\|name; IP/prefix only). |
| `nbox_get` | One object by `kind` (device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range, tenant, contact, provider) + `ref`; `vrf`/`site`/`group` disambiguate. |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix. |
| `nbox_next_prefix` | Next available child prefix(es) of a given length. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_list_tags` | List tags. |

A loopback HTTP transport ships in the default build (behind the `http` cargo
feature, on by default; `--no-default-features` for stdio-only):
`nbox serve --http 127.0.0.1:8080`, optional static bearer — same tools mounted at
`/mcp`, loopback only with `Origin`/`Host` validation. Add `--oidc-issuer` +
`--audience` for OAuth 2.1 resource-server mode (inbound IdP JWT validation,
Protected Resource Metadata, routable bind allowed) — accountability, not per-user
RBAC. See [docs/MCP.md](MCP.md). Per-user NetBox identity bridging, a raw
escape-hatch tool, and MCP resources/prompts are later.

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.
