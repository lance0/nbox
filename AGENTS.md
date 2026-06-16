# nbox for agents

`nbox` is a CLI + TUI for NetBox (DCIM/IPAM). For programmatic/agent use, drive the
CLI subcommands with machine-readable output. The interactive TUI (`nbox` with no
subcommand) is for humans; agents should always pass a subcommand.

## Output

- `--json` / `-o json` — JSON to stdout (pretty by default).
- `--raw` — compact JSON (one line; pairs with `--json`).
- `--envelope` — wrap as `{ "schema_version": 1, "data": <payload> }` for stable parsing.
- `--fields a,b,c` — keep only those top-level fields (per element for arrays).
- `-o csv` — CSV (arrays → table, single objects → `field,value`).

stdout carries only the requested data; logs/diagnostics/errors go to stderr.

Exit codes (stable):

| Code | Meaning                                   |
| ---- | ----------------------------------------- |
| 0    | success                                   |
| 1    | generic error (incl. other API failures)  |
| 2    | usage error (bad arguments)               |
| 3    | authentication / permission (HTTP 401/403)|
| 4    | not found (no object matched)             |
| 5    | ambiguous reference (more than one match) |

Recommended agent invocation: `nbox <cmd> ... --json --envelope` (add `--raw` to
minimize tokens, `--fields` to trim payloads).

## Commands

```
nbox device <name|slug|id>
nbox ip <address> [--vrf <name|slug|rd>]
nbox prefix <cidr> [--vrf <name|slug|rd>]
nbox next-ip <cidr> [--count N] [--vrf <name|slug|rd>]
nbox next-prefix <cidr> [--length L] [--vrf <name|slug|rd>]
nbox vlan <vid|name> [--site <name|slug>] [--group <name|slug>]
nbox interface <device> <interface>
nbox site <name|slug>
nbox rack <name|id>
nbox circuit <cid|id>
nbox aggregate <cidr|id>
nbox asn <number>
nbox ip-range <start|id>
nbox search <query> [--limit N] [--status S] [--site SLUG] [--tenant SLUG] [--role SLUG] [--tag SLUG] [--cols a,b,c] [--partial]
nbox tags
nbox journal <kind> <ref>
nbox open <kind>/<ref>
nbox raw GET <api-path>
nbox status
nbox completions <bash|zsh|fish|powershell|elvish>
```

A reference that matches more than one object across scopes (an address/CIDR in
several VRFs, a VID at several sites) exits `5` and lists the candidates; scope it
with `--vrf` / `--site` / `--group`. `search` fails closed: if any endpoint errors
it exits non-zero rather than return partial results — pass `--partial` for
best-effort results (failed endpoints are reported on stderr).

## Configuration

- Config: `~/.config/nbox/config.toml` (`nbox config init` to create).
- Token: never stored in the config; read from `NBOX_TOKEN` or the profile's
  `token_env` variable. Select a profile with `--profile <name>` or set the active one.
- Targets NetBox 4.2+.

## Examples

```bash
nbox device edge01 --json --envelope
nbox ip 10.44.208.55 --json --fields address,parent_prefix,assigned
nbox search edge --status active --site dc1 -o csv --cols kind,display,url
nbox device edge01 --json | jq '.primary_ip4'
```

## Notes

- Read-only today (v0.1). Safe, diff-confirmed writes are planned for v0.2.
- Filters that an object type can't satisfy cause that type to be skipped in
  `search` (nbox does not send NetBox unknown query params).
