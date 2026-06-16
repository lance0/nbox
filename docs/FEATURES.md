# Features

nbox is a read-only NetBox client (v0.1) — a CLI and a TUI over the same core.

## Lookups

| Command | What |
| ------- | ---- |
| `nbox search <q>` | Parallel search across devices/sites/IPs/prefixes/VLANs. Filters: `--status/--site/--tenant/--role/--tag`, `--limit`, `--cols`, `--partial`. |
| `nbox device <name\|slug\|id>` | Device + interfaces, IPs, cables, VLANs, services. |
| `nbox interface <device> <iface>` | One interface: type, MTU, MAC, mode, VLANs, cable, **cable path** (trace), addresses. |
| `nbox ip <addr> [--vrf]` | IP + most-specific parent prefix (VRF-scoped) and its VLAN/site. |
| `nbox prefix <cidr> [--vrf]` | Prefix with utilization, children, and contained IPs. |
| `nbox next-ip <cidr> [--count] [--vrf]` | Next available address(es). |
| `nbox next-prefix <cidr> [--length] [--vrf]` | Available free block(s). |
| `nbox vlan <vid\|name> [--site] [--group]` | VLAN + referencing prefixes. |
| `nbox site` / `rack` / `circuit` / `aggregate` / `asn` / `ip-range` | Object lookups. |
| `nbox tags` | List tags. |
| `nbox journal <kind> <ref>` | Recent journal entries for an object. |
| `nbox status` | Connection + NetBox/Django/Python versions. |
| `nbox open <kind>/<ref>` | Open an object in the browser. |
| `nbox raw GET <path>` | Raw read-only API request (escape hatch). |

Duplicate references across scopes (an address/CIDR in several VRFs, a VID at
several sites) exit `5` and list the candidates; scope with `--vrf`/`--site`/`--group`.

## Output

Every data command takes `-o plain|json|csv` (`--json` is shorthand). JSON adds
`--fields a,b,c`, `--raw` (compact), and `--envelope` (`{schema_version, data}`).
stdout stays clean for piping; logs/errors go to stderr. See [AGENTS.md](../AGENTS.md)
for the machine-readable surface and exit codes.

## TUI

`nbox` (no subcommand) launches the TUI: `/` search, `:` palette, `Enter` open,
`o` browser, `y` copy, `t` theme, device tabs `i`/`p`/`c`/`v`/`s`, recents on the
home screen, optional auto-refresh (`[ui].refresh_secs`).

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.
