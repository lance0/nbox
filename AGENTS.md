# nbox for agents

`nbox` is a CLI + TUI for NetBox (DCIM/IPAM). For programmatic/agent use, drive the
CLI subcommands with machine-readable output. The interactive TUI (`nbox` with no
subcommand) is for humans; agents should always pass a subcommand.

## Output

- `--json` / `-o json` — JSON to stdout (pretty by default).
- `--raw` — compact JSON (one line; pairs with `--json`).
- `--envelope` — wrap as `{ "schema_version": 1, "data": <payload> }` for stable parsing.
- `--fields a,b,c` — keep only those top-level fields (per element for arrays).
- `-o csv` — CSV for tabular/list results (e.g. `search`); arrays render as a table. Single objects are rejected (exit 2) — use `--json` or plain.

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
nbox search <query> [--limit N] [--status S] [--site SLUG] [--region SLUG] [--site-group SLUG] [--location SLUG] [--tenant SLUG] [--role SLUG] [--tag SLUG] [--cols a,b,c] [--partial]
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

## MCP server

`nbox serve` runs a read-only MCP server over stdio. An MCP host launches it as a
subprocess and speaks JSON-RPC over stdin/stdout; the tools reuse the CLI's query +
view layer, so they return the same JSON view models. URL/token come from the active
profile (same `--profile` / `--config` flags). JSON-RPC is on stdout; logs go to
stderr. Every tool is annotated read-only.

| Tool | Purpose |
| ---- | ------- |
| `nbox_status` | Connection + NetBox/Django/Python versions (call first to confirm reachability). |
| `nbox_search` | Search devices/sites/IPs/prefixes/VLANs; `query` (required), `limit`, `status`, `site`, `region`, `site_group`, `location`, `tenant`, `role`, `tag` (one scope filter at a time). Find a reference before `nbox_get`. |
| `nbox_get` | One object: `kind` (device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range) + `ref`; `vrf`/`site`/`group` disambiguate (an ambiguous ref returns the candidates). |
| `nbox_get_interface` | One interface on a device: config, addresses, cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix (nothing reserved); `count`, `vrf`. |
| `nbox_next_prefix` | Available child prefix(es) in a prefix; `length` for a block of a size, else all free blocks; `vrf`. |
| `nbox_journal` | Recent journal entries for an object (`kind`/`ref` as `nbox_get`). |
| `nbox_list_tags` | List tags (name, slug, color, usage count) — valid `tag` values for `nbox_search`. |

An HTTP transport ships in the default build (behind the `http` cargo feature,
which is on by default; `--no-default-features` for stdio-only):
`nbox serve --http 127.0.0.1:8080`, optional `--http-token` — same tools at
`/mcp`, loopback only, with `Origin`/`Host` validation. Add `--oidc-issuer <URL>`
+ `--audience <VALUE>` for OAuth 2.1 resource-server mode: inbound IdP JWTs are
validated on `/mcp` (alg allowlist, `iss`/`aud`/`exp`, `nbox:read` scope), a
routable bind is allowed (TLS terminates in front), and Protected Resource
Metadata is served at `/.well-known/oauth-protected-resource`. The last hop to
NetBox still uses the local profile token; per-user NetBox identity bridging, a
raw escape-hatch tool, and MCP resources/prompts are later. See `docs/MCP.md`.

## Configuration

- Config: `~/.config/nbox/config.toml` (`nbox config init` to create).
- Token: never stored in the config; read from `NBOX_TOKEN` or the profile's
  `token_env` variable. Select a profile with `--profile <name>` or set the active one.
- Logging: quiet by default (warnings to stderr). `--log-level` / `NBOX_LOG` /
  `RUST_LOG` set verbosity; `--log-file <PATH>` (or config `log_file`) also tees
  `tracing` output to a file. stdout stays data-only on every path.
- Targets NetBox 4.2+.

## Examples

```bash
nbox device edge01 --json --envelope
nbox ip 10.44.208.55 --json --fields address,parent_prefix,scope,scope_type,assigned
nbox search edge --status active --site dc1 -o csv --cols kind,display,url
nbox device edge01 --json | jq '.primary_ip4'
```

## Notes

- Read-only today (v0.1). Safe, diff-confirmed writes are planned for v0.2.
- Filters that an object type can't satisfy cause that type to be skipped in
  `search` (nbox does not send NetBox unknown query params).
- Scope fields (NetBox 4.2+ polymorphic scope): `prefix` and `vlan` carry
  `scope` (the scope object's name, for any scope type) and `scope_type` (a
  friendly label — `site`, `location`, `region`, `site-group`, or the raw
  content type for anything else). `ip` derives `scope`/`scope_type` from its
  most-specific parent prefix. Each is omitted when there is no scope. (There is
  no `site` field on these views — use `scope`/`scope_type`.)
- A `vlan` that belongs to a *VLAN group* additionally carries `group_scope` and
  `group_scope_type`: a VLAN group is itself polymorphically scoped (the VLAN is
  not), so this is the group's scope, surfaced separately from the VLAN's own
  `scope`. Populated by one follow-up fetch of the group, only when the VLAN has
  a group and that group is scoped; both fields are omitted otherwise.
