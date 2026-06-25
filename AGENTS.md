# nbox for agents

`nbox` is a CLI + TUI for NetBox (DCIM/IPAM). For programmatic/agent use, drive the
CLI subcommands with machine-readable output. The interactive TUI (`nbox` with no
subcommand) is for humans; agents should always pass a subcommand. Pass `--no-tui`
to make that a hard guarantee: any invocation that would launch the TUI (a bare
`nbox`, or `nbox tui`) refuses with a usage error (exit 2) instead of blocking on a
terminal.

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
nbox ip <address> [--vrf <name|slug|rd>]  # surfaces nat_inside/nat_outside (NetBox 4.6) when set
nbox prefix <cidr> [--vrf <name|slug|rd>]
nbox next-ip <cidr> [--count N] [--vrf <name|slug|rd>]
nbox next-prefix <cidr> [--length L] [--vrf <name|slug|rd>]
nbox vlan <vid|name> [--site <name|slug>] [--group <name|slug>]
nbox interface <device> <interface>
nbox site <name|slug>
nbox rack <name|id>
nbox rack-group <slug|name|id>      # NetBox 4.6+
nbox circuit <cid|id>                 # JSON: `terminations` (A/Z), each path hop a `device` ref + a `diagram`
nbox virtual-circuit <cid|id>        # JSON: `terminations` (multi-point interface refs); NetBox 4.2+
nbox provider <slug|name|id>
nbox vm-type <slug|name|id>          # NetBox 4.6+
nbox aggregate <cidr|id>
nbox asn <number>
nbox ip-range <start|id>
nbox tenant <slug|name|id>
nbox contact <name|id>
nbox vm <name|id>
nbox cluster <name|id>
nbox vrf <name|rd|id>
nbox route-target <name|id>
nbox mac <addr>                   # any common form (aa:bb:cc:dd:ee:ff, AABB.CCDD.EEFF, …) is normalized; reverse-resolves to the carrying interface(s)/device(s)
nbox search <query> [--limit N] [--status S] [--site <name|slug|id>] [--region <name|slug|id>] [--site-group <name|slug|id>] [--location <name|slug|id>] [--tenant SLUG] [--role SLUG] [--tag SLUG] [--owner <name>] [--owner-group <name>] [--vrf <id|rd|name>] [--cols a,b,c] [--partial]
nbox tags
nbox tagged <tag>               # objects carrying a tag, across kinds (NetBox 4.3+
                                  # `/api/extras/tagged-objects/`); tag = id|name|slug
nbox journal <kind> <ref>         # kinds: device, ip, prefix, vlan, site, rack, rack-group, circuit,
                                  # virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm,
                                  # vm-type, cluster, vrf, route-target, mac, interface (<device>/<name>)
nbox history <kind> <ref> [--diff]  # system audit log (create/update/delete, who + when) for an object —
                                  # `/api/core/object-changes/` (NetBox 4.x); distinct from `journal`
                                  # (operator notes). `--diff` shows the full before/after JSON for the
                                  # newest change (implies --limit 1). Same kind set as `journal`.
nbox open <kind>/<ref>
nbox raw GET <api-path>          # path with or without /api/, e.g. dcim/devices/?limit=1
nbox status                       # NetBox/Django/Python versions, api routing,
                                  # capabilities, and a token-validity preflight
                                  # (NetBox 4.5+ `/api/authentication-check/`)
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
stderr. Every tool is annotated read-only. `nbox serve --print-config` prints the
paste-ready `mcpServers` JSON (absolute binary path, echoed `--profile`/`--config`,
placeholder token) and exits — no server start, no NetBox connection.

| Tool | Purpose |
| ---- | ------- |
| `nbox_status` | Connection + active backend capabilities + NetBox/Django/Python versions **and a token-validity preflight** (NetBox 4.5+): the `token` field is `valid`/`invalid`/`unverified` (the authenticated user on `valid`). Call first to confirm reachability, a valid token, and inspect `capabilities`. |
| `nbox_search` | Search devices/sites/racks/rack-groups/IPs/prefixes/VLANs/circuits/virtual-circuits/aggregates/ASNs/IP ranges/tenants/contacts/providers/VMs/VM-types/clusters/VRFs/route-targets; `query` (required), `limit`, `status`, `site`, `region`, `site_group`, `location`, `tenant`, `role`, `tag` (one scope filter at a time), `owner`/`owner_group` (4.5+; user/group by name), `vrf` (id\|rd\|name; filters IP/prefix results only). Find a reference before `nbox_get`; a result's `kind` (e.g. `ip_address`) feeds straight into `nbox_get`, which accepts it as an alias for `ip`. |
| `nbox_get` | One object: `kind` (device, ip, prefix, vlan, site, rack, rack_group, circuit, virtual_circuit, aggregate, asn, ip_range, tenant, contact, provider, vm, vm_type, cluster, vrf, route_target, mac, interface) + `ref`; `vrf`/`site`/`group` disambiguate (an ambiguous ref returns the candidates). |
| `nbox_get_interface` | One interface on a device: config, addresses, cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix (nothing reserved); `count`, `vrf`. |
| `nbox_next_prefix` | Available child prefix(es) in a prefix; `length` for a block of a size, else all free blocks; `vrf`. |
| `nbox_journal` | Recent journal entries for an object (`kind`/`ref` as `nbox_get`). |
| `nbox_history` | Change history (system audit log: create/update/delete, who + when) for an object (`kind`/`ref` as `nbox_get`); `/api/core/object-changes/` (NetBox 4.x). Distinct from `nbox_journal` (operator notes): this is the system-recorded audit trail. Each row includes the top-level fields that changed (pre vs post); pass `diff=true` (pair with a small `limit`, e.g. 1) to include the full before/after JSON payloads per row. |
| `nbox_list_tags` | List tags (name, slug, color, usage count) — valid `tag` values for `nbox_search`. |
| `nbox_tagged` | Objects carrying a tag, across kinds (NetBox 4.3+); `tag` (id\|name\|slug). Cross-kind reverse lookup — unlike `nbox_search --tag`, which narrows a free-text search per-endpoint. |
| `nbox_cache_clear` | Drop nbox's local read cache so the next lookups fetch fresh from NetBox (read-only w.r.t. NetBox; idempotent). |

The same objects are also exposed as MCP **resources** via one template,
`nbox://{kind}/{ref}` (e.g. `nbox://device/edge01`, `nbox://ip/10.0.0.1`), for
hosts that browse/attach resources instead of calling tools. Reading one returns
the same JSON view as `nbox_get`, routed through the same view layer; `kind`/`ref`
follow `nbox_get` (percent-encode a `ref` with `/`, e.g. a CIDR). It's a template,
not a static list, so `resources/list` is empty.

A small catalog of **read-only investigation prompts** (`prompts/list`/
`prompts/get`) ships with it: `ip_utilization_audit`, `cable_path_trace`,
`find_stale_prefixes`, `object_change_review`. Each returns a user-role message
with a structured plan naming the exact nbox tools to call (incl. `nbox_history`),
tailored to the supplied arguments — a plan, not data (no NetBox round-trip).

An HTTP transport ships in the default build (behind the `http` cargo feature,
which is on by default; `--no-default-features` for stdio-only):
`nbox serve --http 127.0.0.1:8080`, optional `--http-token` — same tools at
`/mcp`, loopback only, with `Origin`/`Host` validation. Add `--oidc-issuer <URL>`
+ `--audience <VALUE>` for OAuth 2.1 resource-server mode: inbound IdP JWTs are
validated on `/mcp` (alg allowlist, `iss`/`aud`/`exp`, `nbox:read` scope), a
routable bind is allowed (TLS terminates in front), and Protected Resource
Metadata is served at `/.well-known/oauth-protected-resource`. The HTTP `/mcp`
path also has an ops layer: a structured audit log (one `tracing` event per
authenticated request under the target `nbox::audit` — WHO/WHAT/WHEN/OUTCOME, no
token ever; off under the default `warn` filter, opt in with
`NBOX_LOG=…,nbox::audit=info`) and an opt-in per-caller rate limit
(`--rate-limit <N>` / `[serve].rate_limit`, keyed `sub`→`client_id`→peer IP, over
the limit → `429`+`Retry-After`; `0`/absent = off). This is **read-only Pattern 3**:
the last hop to NetBox still uses the one local profile token, so the audit log is
accountability, not per-user RBAC — trusted single-team read-only only. Per-user
NetBox identity bridging (the Pattern 2 vault, v2), a raw escape-hatch tool, and
full per-prompt argument schemas are later. See `docs/MCP.md`.

## Configuration

- Config: `~/.config/nbox/config.toml` (`nbox config init` to create).
- Token: resolved in order: the profile's `token_env` variable (if set & present)
  → `NBOX_TOKEN` → the profile's config `token` → none. Env always overrides
  saved tokens. Inspect the active source with `nbox config token status` (never
  prints the token).
  Select a profile with `--profile <name>` or set the active one.
- Backends: REST is canonical. GraphQL is an opt-in per-surface accelerator set
  under `[profiles.<name>.api]` (`vrf`/`route_target` = `rest`|`graphql`; missing = REST). nbox
  probes `/graphql/`, adapts to NetBox 4.2/4.3/4.5+ filter/pagination shapes, and
  falls back to REST (with the reason in `nbox status`) when a surface isn't
  supported. The output shape is identical either way. **Search is always REST** —
  NetBox GraphQL has no equivalent to REST's full-text `q`, so a `search =
  "graphql"` preference transparently falls back. The old coarse `backend = …` key
  was removed and is rejected. Identity resolution and all other operations remain
  REST-backed.
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

- Read-only today. Safe, diff-confirmed writes are planned for a later release.
- Filters that an object type can't satisfy cause that type to be skipped in
  `search` (nbox does not send NetBox unknown query params).
- `owner` (NetBox 4.5+): a native owner field (user or group) surfaced on most
  detail views as a friendly label, omitted when absent (byte-identical for
  pre-4.5 objects). In `search`, `--owner`/`--owner-group` filter by user/group
  name; owner is polymorphic (user **or** group) so the two are separate filters,
  and both are silently ignored on releases that carry no owner data.
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
