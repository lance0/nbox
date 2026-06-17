# Features

nbox is a read-only NetBox client (v0.1) — a CLI and a TUI over the same core.

## Lookups

| Command | What |
| ------- | ---- |
| `nbox search <q>` | Parallel search across devices/sites/IPs/prefixes/VLANs/circuits/aggregates/ASNs/IP-ranges. Filters: `--status/--site/--tenant/--role/--tag`, `--limit`, `--cols`, `--partial`. |
| `nbox device <name\|slug\|id> [--journal]` | Device + interfaces, IPs, cables, VLANs, services. |
| `nbox interface <device> <iface>` | One interface: type, MTU, MAC, mode, VLANs, cable, **cable path** (trace), addresses. |
| `nbox ip <addr> [--vrf] [--journal]` | IP + most-specific parent prefix (VRF-scoped) and its VLAN plus the prefix's `scope`/`scope_type` (site, location, region, …). |
| `nbox prefix <cidr> [--vrf] [--journal]` | Prefix with utilization, children, and contained IPs. |
| `nbox next-ip <cidr> [--count] [--vrf]` | Next available address(es). |
| `nbox next-prefix <cidr> [--length] [--vrf]` | Available free block(s). |
| `nbox vlan <vid\|name> [--site] [--group] [--journal]` | VLAN + referencing prefixes. |
| `nbox site` / `rack` / `circuit` / `aggregate` / `asn` / `ip-range` `[--journal]` | Object lookups. |
| `nbox tags` | List tags. |
| `nbox journal <kind> <ref>` | Recent journal entries for an object. `--journal` on a detail lookup (device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range) folds the most recent entries inline (default 5); `--journal-limit <N>` overrides the cap and implies `--journal`. |
| `nbox status` | Connection + NetBox/Django/Python versions. |
| `nbox open <kind>/<ref>` | Open an object in the browser. Kinds: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range. |
| `nbox raw GET <path>` | Raw read-only API request (escape hatch). |

Duplicate references across scopes (an address/CIDR in several VRFs, a VID at
several sites) exit `5` and list the candidates; scope with `--vrf`/`--site`/`--group`.

`search --site <ref>` resolves the site once (by slug, name, or id) and filters
prefixes by site scope — NetBox 4.2 replaced the prefix `site` field with the
polymorphic `scope`, so prefixes are matched on `scope_type=dcim.site` +
`scope_id`, not the dead `?site=` filter. An unknown site is a not-found error
(exit `4`), not a silent empty result. Other endpoints (devices, VLANs, …) take
the site reference directly; endpoints that can't filter by site are skipped.
Only the site scope is filtered today; region/site-group/location are not yet.

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
| `nbox_search` | Search devices/IPs/prefixes/VLANs/sites/circuits/aggregates/ASNs/IP-ranges; `query`, `limit`, `status`, `site`, `tenant`, `role`, `tag`. |
| `nbox_get` | One object by `kind` (device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range) + `ref`; `vrf`/`site`/`group` disambiguate. |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix. |
| `nbox_next_prefix` | Next available child prefix(es) of a given length. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_list_tags` | List tags. |

HTTP transport, OAuth, a raw escape-hatch tool, and MCP resources/prompts are later.

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.
