# Features

nbox is a NetBox client — a CLI and a TUI over the same core. Reads are the
default; a narrow safe-write foundation (ADR-0001) adds seven plan-first
commands behind `--allow-writes` + confirmation — `interface … set
description` and `device … set status` (`PATCH`), `ip reserve <prefix>` /
`prefix reserve <cidr>` / `ip-range reserve <start|id>` (three `allocate`
`POST`s), and `tag add`/`remove <type> <name> <tag>` (a `PATCH` to the `tags`
array on any object). The same safe-write foundation is also exposed as MCP
write tools (`nbox_plan_write`/`nbox_apply_write`) in two explicit modes: local
stdio with `[serve].local_writes` / `--local-writes` using the active profile
token, or shared HTTP/OIDC with `[serve].allow_writes` / `--allow-writes` plus
the per-user credential vault.

## Lookups

| Command | What |
| ------- | ---- |
| `nbox search <q>` | Parallel search across devices/sites/racks/rack-groups/IPs/prefixes/VLANs/circuits/virtual-circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers/VMs/VM-types/clusters/VRFs/route-targets. Filters: `--status/--site/--region/--site-group/--location/--tenant/--role/--tag/--owner/--owner-group/--vrf`, `--limit`, `--cols`, `--partial`. |
| `nbox device <name\|slug\|id> [--journal]` | Device + interfaces, IPs, cables, VLANs, services. `set status <value>` is a safe write (ADR-0001): status validated live via OPTIONS, behind `--allow-writes` + confirm. |
| `nbox interface <device> <iface>` | One interface: type, MTU, MAC, mode, VLANs, cable, **cable path** (an A↔Z trace diagram naming the device at each end), addresses. `set description "…"` is a safe write (ADR-0001), behind `--allow-writes` + confirm. |
| `nbox ip <addr> [--vrf] [--journal]` | IP + most-specific parent prefix (VRF-scoped) and its VLAN plus the prefix's `scope`/`scope_type` (site, location, region, …); surfaces `nat_inside`/`nat_outside` (NetBox 4.6) when set. `reserve <prefix> [--description] [--dns-name] [--count N]` is a safe write (ADR-0001): `POST` to `available-ips`, behind `--allow-writes` + confirm. `--count N` allocates N IPs atomically (one list-body POST, all-or-nothing); any failure exits 1. `--dry-run` previews the candidate; the receipt's `object` is the created IP (or array for `--count > 1`). |
| `nbox prefix <cidr> [--vrf] [--journal]` | Prefix with utilization, children, and contained IPs. `prefix reserve [--length N] [--description]` is a safe write (ADR-0001): `POST` to `available-prefixes`, behind `--allow-writes` + confirm. `--dry-run` previews the candidate; the receipt's `object` is the created prefix. |
| `nbox next-ip <cidr> [--count] [--vrf]` | Next available address(es) (read-only preview). |
| `nbox next-prefix <cidr> [--length] [--vrf]` | Available free block(s). |
| `nbox vlan <vid\|name> [--site] [--group] [--journal]` | VLAN + referencing prefixes, plus the VLAN's own `scope`/`scope_type` and, when it belongs to a scoped VLAN group, the group's `group_scope`/`group_scope_type`. |
| `nbox ip-range <start\|id>` | IP range. `reserve [--description] [--dns-name] [--count N]` is a safe write (ADR-0001): `POST` to `available-ips`, behind `--allow-writes` + confirm. `--count N` allocates N IPs atomically (one list-body POST, all-or-nothing); any failure exits 1. `--dry-run` previews the candidate; the receipt's `object` is the created IP (or array for `--count > 1`). |
| `nbox tenant <slug\|name\|id>` | Tenant: group, description, relation counts, tags, custom fields. |
| `nbox contact <name\|id>` | Contact: title, phone, email, address, link, group, tags, custom fields. |
| `nbox provider <slug\|name\|id>` | Provider: ASNs, accounts, description, circuit count, tags, custom fields. |
| `nbox vm <name\|id>` | Virtual machine: status, role, cluster, device, platform, vcpus, memory, disk, primary IPs, tenant, site, description, owner, tags, custom fields. |
| `nbox vm-type <slug\|name\|id>` | VM type (NetBox 4.6+): default platform/vCPUs/memory, VM count, owner, tags, custom fields. |
| `nbox cluster <name\|id>` | Cluster: type, group, status, tenant, scope (site/region/…), device + VM counts, description, tags, custom fields. |
| `nbox vrf <name\|rd\|id>` | VRF as a routing context: summary (RD, tenant, enforce-unique, import/export route targets, counts) plus its prefix tree and scoped addresses. |
| `nbox route-target <name\|id>` | Route target (e.g. 65000:100): tenant/description plus the VRFs that import and export it (navigable). |
| `nbox mac <addr>` | Reverse-resolve a MAC to the interface(s)/device(s) that carry it (NetBox 4.2+). Any common form is normalized (`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF`, `aa-bb-…`, `aabbccddeeff`); a non-MAC is a usage error, several carrying interfaces are ambiguous. |
| `nbox tags` | List tags. |
| `nbox tagged <tag>` | Objects carrying a tag, across kinds (NetBox 4.3+ `/api/extras/tagged-objects/`); tag = id/name/slug. |
| `nbox tag add <type> <name> <tag>` | Add a tag to any taggable object — a safe write (ADR-0001): `PATCH` to the `tags` array, behind `--allow-writes` + confirm. Tag resolves by id/name/slug; target resolves like `nbox <kind> <ref>`. No-op if already present. |
| `nbox tag remove <type> <name> <tag>` | Remove a tag from any taggable object — a safe write (ADR-0001): `PATCH` to the `tags` array, behind `--allow-writes` + confirm. Mirrors `tag add`; no-op if already absent. |
| `nbox journal <kind> <ref>` | Recent journal entries for an object. Kinds: device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, vm-type, cluster, vrf, route-target, interface (`<device>/<name>`). `--journal` on a detail lookup folds the most recent entries inline (default 5); `--journal-limit <N>` overrides the cap and implies `--journal`. (`tenant`/`contact`/`provider`/`vm`/`vm-type`/`cluster`/`vrf`/`route-target`/`interface`/`virtual-circuit` have no inline `--journal` flag — use `nbox journal`.) |
| `nbox history <kind> <ref>` | Change history (system audit log: create/update/delete, who + when) for an object, newest first. Same kind set as `journal`. `/api/core/object-changes/` (NetBox 4.x) — distinct from `journal` (operator notes). Each row includes the top-level fields that changed (pre vs post), not the full before/after JSON. |
| `nbox status` | Connection + per-surface `api` routing (configured/effective) + capabilities + NetBox/Django/Python versions + a token-validity preflight (`token`: `valid`/`invalid`/`unverified`; NetBox 4.5+). |
| `nbox open <kind>/<ref>` | Open an object in the browser. Kinds: device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, aggregate, asn, ip-range, tenant, contact, provider, vm, vm-type, cluster, vrf, route-target, mac, and `interface/<device>/<name>` (the interface name may contain slashes, e.g. `xe-0/0/1`). |
| `nbox raw GET <path>` | Raw read-only API request (escape hatch). |

## Structured exports

| Command | What |
| ------- | ---- |
| `nbox export prometheus-sd --prefix <cidr> [--vrf] [--port N]` | Emit Prometheus file-based service-discovery JSON (`[{"targets": ["ip:port"], "labels": {...}}]`) for IPs assigned within a prefix. Reuses the read engine: resolves the prefix (with `--vrf` disambiguation), lists its member IPs, enriches each with its assigned device's site/role/status via one `id__in` fetch, and groups targets by device. Pipe straight to a file Prometheus scrapes. |
| `nbox export prometheus-sd --tag <slug> [--port N]` | Same SD JSON, sourced from IPs carrying a tag (IP-addresses `?tag=` filter). |
| `nbox export address-list (--prefix <cidr> [--vrf] \| --tag <slug>) [--family 4\|6] [--summarize] [--format json\|plain]` | Emit a firewall/blocklist address list. A prefix source lists its assigned IPs as host entries (`/32`, `/128` — the interface mask is dropped); a tag source lists the IPs *and* whole prefixes carrying the tag. Entries are de-duplicated and sorted; `--summarize` aggregates contiguous entries into the minimal covering set (e.g. two /25s → one /24); `--family` keeps one IP family. Output is a JSON array of CIDR strings (default) or `--format plain` (one per line) for ipset/nftables/pf includes. |
| `nbox export device-inventory [--site] [--role] [--tag] [--status] [--manufacturer] [--format json\|csv]` | Emit a device inventory — one record per device (`name`, `status`, `role`, `site`, `model`, `platform`, `serial`, `asset_tag`, `rack`, `primary_ip`, `tenant`, `tags`), filtered by any combination of the slug/value flags (ANDed). JSON array (default) or `--format csv` for spreadsheets. The JSON keys and the CSV columns line up one-to-one. |

Targets are `host:port` (default port `9100`, the conventional `node_exporter`
port; IPv6 hosts are bracketed, `[2001:db8::1]:9100`). Labels per group:
`device`, `site`, `role`, `status`, `tags` (comma-joined, de-duplicated, sorted;
the union of each IP's own tags and its device's). `site`/`role`/`status` come
from the IP's assigned device, so IPs with no assigned device form a single
`device=""` group (no `site` — there is no device to derive one from). `--prefix`
and `--tag` are mutually exclusive. A source larger than the gather cap
(5000 IPs) is truncated with a warning on stderr. Output is a compact JSON array
on stdout — pipe-safe, no envelope.

Every detail lookup surfaces the object's `tags` (joined slugs in plain output, a
`tags` array in `--json`), dropped when the object has none, plus its non-empty
custom fields as `cf.<name>`. `owner` (NetBox 4.5+) — a native owner (user or
group) — is surfaced on most detail views as a friendly label and omitted when
absent (byte-identical for pre-4.5 objects).

Duplicate references across scopes (an address/CIDR in several VRFs, a VID at
several sites) exit `5` and list the candidates; scope with `--vrf`/`--site`/`--group`.

`search --site/--region/--site-group/--location <ref>` resolves the reference
once (by slug, name, or **id**) to a numeric id and filters prefixes by that
scope — NetBox 4.2 replaced the prefix `site` field with the polymorphic `scope`,
so scoped endpoints are filtered out-of-band rather than through the dead
`?site=` prefix filter. `--site` is exact (`scope_type=dcim.site` +
`scope_id=<id>` on prefixes/clusters; `site_id=<id>` where available). The
hierarchical scopes use NetBox's tree-aware id filters where the endpoint
supports them: `region_id`, `site_group_id`, and `location_id` include the
selected node and its descendants. At most **one** scope flag may be set (the
prefix `scope` is a single type+id); passing more than one is a usage error
(exit `2`). An unknown reference is a not-found error (exit `4`), not a silent
empty result. Non-prefix endpoints filter by the **resolved id**, never a raw
value (the plain `?site=` param wants a slug, so a `--site` given as an id or
display name would silently match nothing): clusters carry the same scoped model
filters as prefixes; devices and racks honor every scope via
`site_id`/`region_id`/`site_group_id`/`location_id`; VLANs and VMs honor `--site`
via `site_id`; endpoints that can't filter by a given scope are skipped rather
than sent a dead param.

`search --vrf <id|rd|name>` resolves the VRF once (numeric id, then RD, then
name — VRFs have no slug) and filters the VRF-capable endpoints (IPs, prefixes)
by `vrf_id=`. Endpoints that carry no VRF (devices, sites, VLANs, circuits, …)
are skipped for this filter (queried unfiltered, not dropped). `--vrf` is
orthogonal to the scope filters above — both may be set, and NetBox ANDs them on
prefixes. An unknown VRF is a not-found error (exit `4`), not a silent empty
result.

`search --owner <name>` / `--owner-group <name>` (NetBox 4.5+) filter by owner
— a user (by username) or a group (by name). Owner is polymorphic (user **or**
group), so the two are separate filters; both are passed straight through as
`owner=`/`owner_group=` params on every search endpoint (no resolution step — the
server matches by name). On releases that carry no owner data the filters are
silently ignored (queried unfiltered, not dropped).

Profiles opt GraphQL-capable views into NetBox GraphQL under
`[profiles.<name>.api]`: `vrf = "graphql"` bundles the VRF prefix/address section,
and `route_target = "graphql"` bundles importing/exporting VRFs. A GraphQL surface
returns the same normalized shape, falls back to REST when the live schema can't
support it, and retries REST if the runtime bundle fails (for example, a low
GraphQL query-depth cap). nbox probes `/graphql/` at runtime and adapts to the
schema: NetBox 4.2's unpaginated list fields, NetBox 4.3+'s offset pagination, and
NetBox 4.5+'s lookup-wrapper filters are all shaped from introspection rather than
version strings. **Search is always REST** — NetBox's GraphQL API has no equivalent
to REST's full-text `q`, so a `search = "graphql"` preference transparently falls
back. REST stays canonical and powers search, identity resolution, raw reads,
journals, and available-IP/prefix commands. (The old coarse `backend = …` profile
key was removed.)

## Output

Every data command takes `-o plain|json` (`--json` is shorthand). JSON adds
`--fields a,b,c`, `--raw` (compact), and `--envelope` (`{schema_version, data}`).
`-o csv` is for tabular/list results (e.g. `search`); single objects are rejected
(use `--json` or plain). stdout stays clean for piping; logs/errors go to stderr.
See [AGENTS.md](../AGENTS.md) for the machine-readable surface and exit codes.

## TUI

`nbox` (no subcommand) launches the TUI: a three-pane home with a navigation
rail of browsable kinds, a results list, and a live detail preview. First run
with no config drops into a guided onboarding wizard instead.

On a browse kind, `/` is a server-side filter on that list rather than a search:
a name substring (`name__ic`) for devices/sites/racks/VLANs/VRFs/route-targets,
or network containment for prefix (`within_include`) and IP (`parent`) browse
(type a CIDR, Enter; the title reads `within "10.0.0.0/24"`).

The home screen lists recently opened objects (deduped, most recent first) when
there are no search results; `Enter` reopens one. Set `[ui].refresh_secs` to
auto-refresh the current search on an interval (off by default). A detail served
from the [read cache](CONFIG.md#cache-cache) shows "cached Ns ago"; `r` fetches
fresh.

### Keybindings

| Key | Action |
|-----|--------|
| `/` | search |
| `:` | command palette |
| `f` / `F` | filter results / clear filters |
| `Tab` / `Shift+Tab` | switch pane focus (or cycle detail tabs) |
| `j` / `k` | move selection / scroll detail (on the nav rail, live-browse the kind) |
| `g` / `G` | top / bottom |
| `PgUp` / `PgDn` | page up / down |
| `Enter` | open selected object |
| `o` | open in browser |
| `y` | copy current item label (desktop clipboard, or OSC 52 over SSH/headless terminals) |
| `R` | related objects (jump between connected objects) |
| `D` | overview dashboard |
| `T` | prefix tree (`Space` / `←` / `→` collapse/expand) |
| `t` | cycle theme |
| `r` | refresh |
| `P` / `Ctrl+P` | switch profile (cycle forward / backward) |
| `S` | open the Config modal (profiles + settings) |
| `b` / `Esc` | back / clear search |
| `i p c v s` | device tabs (interfaces / IPs / cables / VLANs / services); `j`/`k` + `Enter` opens a row — interfaces/cables open the interface detail (with its cable-path A↔Z diagram) |
| `e` | rack elevation |
| `u` | dismiss update notice |
| `?` / `F1` | help |
| `q` / `Ctrl+c` | quit |

### Command palette

`:` accepts `device`/`ip`/`prefix`/`vlan`/`site`/`vrf <ref>`, `find <q>` (or
bare text), `filter <key>=<value>`, `open`, `copy`, `theme <name>`,
`profile <name>`, `config`, and `refresh`.

### Switching profiles

`P` / `Ctrl+P` (or `profile <name>` in the palette) switches between the
profiles in your config without restarting: it rebuilds the client for that
instance and re-probes `/api/status/`, so the header flips to the new profile
and its NetBox version. With a single profile the hotkey is a no-op. The cycle
is session-only; it does not rewrite `active_profile` (use
`nbox profile use <name>` for that).

### Config modal

`S` (or `config` in the palette) opens the Config modal to manage profiles
in-app: list them (active marked), and add, edit, select, or delete without
hand-editing `config.toml`. The add/edit form covers `name`, `url`,
`token_env`, `auth_scheme`, and `verify_tls`, plus an optional masked token
field. A typed token is saved to `config.toml` (redacted by display commands;
written user-only on Unix); on an edit, `Ctrl+X` clears the stored token.
`Ctrl+T` test-connects before you commit; `Enter` saves, `Ctrl+G` saves and
switches to it. Unlike the `P` cycle, selecting or adding-and-using a profile
here persists `active_profile`. Deleting the active or last profile is blocked.

`Tab` switches the modal to its **Settings** section, a two-pane editor for
appearance, behavior, connection, cache, and logging keys. Saves are
format-preserving and hot-applied where possible. The keys and navigation are
in [Configuration: UI settings](CONFIG.md#ui-settings).

### Themes

Twelve built-in themes. Set `[ui].theme` in the config, press `t` to cycle, or
use `:theme <name>`:

`default`, `kawaii`, `cyber`, `dracula`, `monochrome`, `matrix`, `nord`,
`gruvbox`, `catppuccin`, `tokyo_night`, `solarized`, `light`.

`light` is the only light-background theme. `NO_COLOR` is honored: when set, the
TUI renders without color and marks the selection with a `>` cursor.

## MCP server

`nbox serve` exposes the same lookups to AI agents over MCP: eleven read tools,
every object as an `nbox://{kind}/{ref}` resource, a small catalog of read-only
investigation prompts, and two opt-in write tools (`nbox_plan_write` /
`nbox_apply_write`) on the same safe-write engine as the CLI. Read-only by
default; stdio by default, with a loopback HTTP transport, an OAuth 2.1
resource-server mode, and NetBox-token pass-through for shared deployments.
Tools, transports, write modes, and host setup are in [MCP server](MCP.md).

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.

## CLI reference

`nbox <command> --help` (or `man nbox-<command>`, see
[Man pages](INSTALL.md#man-pages)) has the full flag list for each command.

```bash
nbox                              # launch the TUI
nbox status                       # connection + backend capabilities + NetBox/Django/Python versions
                                  # + token-validity preflight (NetBox 4.5+)
nbox search <query> [--limit N] [--status/--site/--region/--site-group/--location/--tenant/--role/--tag <v>] [--owner <name>] [--owner-group <name>] [--vrf <id|rd|name>] [--cols a,b,c] [--partial]
                                  # one scope flag at a time; --site is exact, region/site-group/location include descendants where supported.
                                  # --vrf filters IP/prefix results.
                                  # --owner/--owner-group filter by user/group name (NetBox 4.5+).
nbox device <name-or-id> [--journal] [--journal-limit N]
  device <name> set status <value>                    # write: status validated live via OPTIONS; --dry-run | --allow-writes --confirm [--message]
nbox ip <address> [--vrf <name>] [--journal]    # --vrf disambiguates duplicates across VRFs
                                  # shows nat_inside/nat_outside (NetBox 4.6) when set
  ip reserve <cidr> [--vrf <name>] [--description "…"] [--dns-name "…"] [--count N]
                                  # write: reserve the next available IP (POST available-ips); --dry-run | --allow-writes --confirm [--message]
                                  # --count N: reserve N IPs atomically (one list-body POST, all-or-nothing); any failure exits 1
nbox prefix <cidr> [--vrf <name>] [--journal]   # includes utilization + children when present
  prefix reserve [--length L] [--vrf <name>] [--description "…"]
                                  # write: reserve the next available child prefix (POST available-prefixes); --dry-run | --allow-writes --confirm [--message]
nbox next-ip <cidr> [--count N] [--vrf <name>]        # next available address(es)
nbox next-prefix <cidr> [--length L] [--vrf <name>]   # available free block(s)
nbox vlan <vid-or-name> [--site <s>] [--group <g>] [--journal]
nbox site <name-or-slug> [--journal]
nbox rack <name-or-id> [--journal]
nbox rack-group <slug-or-name-or-id>             # NetBox 4.6+
nbox circuit <cid-or-id> [--journal]              # incl. its A↔Z terminations + path
nbox virtual-circuit <cid-or-id>                   # incl. its multi-point terminations (4.2+)
nbox provider <slug-or-name-or-id>
nbox aggregate <cidr-or-id> [--journal]
nbox asn <number> [--journal]
nbox ip-range <start-or-id> [--journal]
  ip-range reserve [--description "…"] [--dns-name "…"] [--count N]
                                  # write: reserve the next available IP in an IP range (POST available-ips); --dry-run | --allow-writes --confirm [--message]
                                  # --count N: reserve N IPs atomically (one list-body POST, all-or-nothing); any failure exits 1
nbox tenant <slug-or-name-or-id>
nbox contact <name-or-id>
nbox vm <name-or-id>
nbox vm-type <slug-or-name-or-id>                 # NetBox 4.6+
nbox cluster <name-or-id>
nbox vrf <name-or-rd-or-id>
nbox route-target <name-or-id>
nbox mac <addr>                   # reverse-resolve to the carrying interface(s)/device(s)
nbox interface <device> <interface>
  interface <device> <interface> set description "…"   # write: --dry-run | --allow-writes --confirm [--message]
nbox tags                         # list tags (slug, name, count)
nbox tagged <tag>                 # objects carrying a tag, across kinds
                                  # (NetBox 4.3+; tag = id|name|slug)
nbox tag add <type> <name> <tag>   # write: add a tag to any object (PATCH tags array)
nbox tag remove <type> <name> <tag>  # write: remove a tag from any object (PATCH tags array)
nbox journal <kind> <ref>         # recent journal entries for an object
                                  # kinds incl. interface as <device>/<name>
                                  # --journal folds recent entries into a detail lookup (cap 5)
                                  # --journal-limit N overrides the cap (implies --journal)
nbox history <kind> <ref> [--diff]  # system audit log (create/update/delete, who + when)
                                  # /api/core/object-changes/ (NetBox 4.x); distinct from journal
nbox open <kind>/<ref>            # device, ip, prefix, vlan, site, rack, rack-group, circuit, virtual-circuit, provider,
                                  # aggregate, asn, ip-range, tenant, contact, vm, vm-type, cluster, vrf, route-target,
                                  # and interface/<device>/<name> (the name may contain slashes,
                                  # e.g. xe-0/0/1)
nbox raw GET <api-path>           # raw read-only API request (escape hatch)
nbox export <prometheus-sd|address-list|device-inventory>
                                  # structured read-only exports (Prometheus SD JSON, firewall/blocklist
                                  # address list, device inventory) — fixed shapes for scripting
nbox serve [--http <addr>]        # read-first MCP server for AI agents (read-only by default;
                                  # --local-writes enables local stdio writes;
                                  # --allow-writes enables shared HTTP/OIDC writes;
                                  # --netbox-token-passthrough forwards each HTTP caller's own NetBox token)
                                  # --print-config prints the paste-ready mcpServers JSON and exits
nbox config <init|path|show|token>    # token: status reports the resolved source (never echoed)
nbox profile <add|use|remove|list|show>
nbox completions <bash|zsh|fish|powershell|elvish>
nbox man [DIR]                    # man pages: `nbox man > nbox.1` (top-level),
                                  # or `nbox man man/` for the full per-subcommand set
```

### Global flags

These apply to every command:

| Flag | Effect |
|------|--------|
| `-o, --output <fmt>` | `plain` (default), `json`, or `csv` (tabular/list results only) |
| `--json` | Shortcut for `-o json` |
| `--fields a,b,c` | JSON: keep only these top-level fields |
| `--raw` | JSON: compact (single line) instead of pretty |
| `--envelope` | JSON: wrap as `{ schema_version, data }` |
| `-p, --profile <name>` | Use a specific profile for this invocation |
| `--config <path>` | Use an alternate config file |
| `--log-level <spec>` | `tracing` filter (`info`, `debug`, `nbox=debug`, …) |
| `--log-file <path>` | Write logs to this file (and stderr); stdout stays clean |
| `--no-tui` | Refuse to launch the interactive TUI (a bare `nbox` or `nbox tui` exits `2` instead) |

Logs go to stderr, never stdout; the level and file precedence are in
[Configuration: Logging](CONFIG.md#logging). Exit codes are stable; see
[Scripting: Exit codes](SCRIPTING.md#exit-codes).
