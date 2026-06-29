# Features

nbox is a NetBox client — a CLI and a TUI over the same core. Reads are the
default; a narrow safe-write foundation (ADR-0001) adds seven plan-first
commands behind `--allow-writes` + confirmation — `interface … set
description` and `device … set status` (`PATCH`), `ip reserve <prefix>` /
`prefix reserve <cidr>` / `ip-range reserve <start|id>` (three `allocate`
`POST`s), and `tag add`/`remove <type> <name> <tag>` (a `PATCH` to the `tags`
array on any object). The same safe-write foundation is also exposed as MCP
write tools (`nbox_plan_write`/`nbox_apply_write`) over the authenticated
HTTP+OIDC transport when `[serve].allow_writes` is set, gated by the per-user
credential vault and running as the calling user; stdio and unauthenticated
transports stay read-only.

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

`nbox serve` defaults to a read-only MCP server over stdio. An MCP host launches it as a
subprocess and speaks JSON-RPC over stdin/stdout; the tools reuse the CLI's query
+ view layer and return the same JSON view models. JSON-RPC on stdout, logs on
stderr; URL/token from the active profile (same `-p`/`--config` flags). The read
tools are annotated read-only; the write tools (`nbox_plan_write`/`nbox_apply_write`,
exposed only over HTTP+OIDC with `--allow-writes`) are not. `nbox serve --print-config` prints the paste-ready
`mcpServers` JSON (absolute binary path, echoed `--profile`/`--config`, placeholder
token) and exits — no server start, no connection; see docs/MCP.md for the per-host
config-file path.

| Tool | What |
| ---- | ---- |
| `nbox_status` | Connection + active backend capabilities + NetBox/Django/Python versions + a token-validity preflight (`token`: `valid`/`invalid`/`unverified`; NetBox 4.5+). |
| `nbox_search` | Search devices/sites/racks/IPs/prefixes/VLANs/circuits/virtual-circuits/aggregates/ASNs/IP-ranges/tenants/contacts/providers/VMs/clusters/VRFs/route-targets; `query`, `limit`, `status`, `site`, `region`, `site_group`, `location`, `tenant`, `role`, `tag`, `owner`/`owner_group` (4.5+; user/group by name), `vrf` (id\|rd\|name; IP/prefix only). |
| `nbox_get` | One object by `kind` (device, ip, prefix, vlan, site, rack, rack_group, circuit, virtual_circuit, aggregate, asn, ip_range, tenant, contact, provider, vm, vm_type, cluster, vrf, route_target, mac, interface) + `ref`; `vrf`/`site`/`group` disambiguate. |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix. |
| `nbox_next_prefix` | Available child prefix(es) of a given length, or all free blocks. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_history` | Change history (system audit log: create/update/delete, who + when) for an object. `/api/core/object-changes/` (NetBox 4.x) — distinct from `nbox_journal` (operator notes). |
| `nbox_list_tags` | List tags. |
| `nbox_tagged` | Objects carrying a tag, across kinds (NetBox 4.3+); `tag` (id\|name\|slug). Cross-kind reverse lookup. |
| `nbox_cache_clear` | Drop nbox's local read cache so the next lookups fetch fresh (read-only w.r.t. NetBox). |
| `nbox_plan_write` | Plan a safe write (interface description, device status, IP/prefix/IP-range reserve, tag add/remove): builds a before/after diff and a confirm token without mutating. Requires `--allow-writes`, the caller's `nbox:write` scope, and a `[serve.vault]` mapping for the caller's OIDC `sub`; rejected over stdio. |
| `nbox_apply_write` | Apply a previously planned write (verifies the confirm token, then executes under the caller's per-user NetBox identity). Same gating as `nbox_plan_write`. |

A loopback HTTP transport ships in the default build (behind the `http` cargo
feature, on by default; `--no-default-features` for stdio-only):
`nbox serve --http 127.0.0.1:8080`, optional static bearer — same tools mounted at
`/mcp`, loopback only with `Origin`/`Host` validation. Add `--oidc-issuer` +
`--audience` for OAuth 2.1 resource-server mode (inbound IdP JWT validation,
Protected Resource Metadata, routable bind allowed) — accountability, not per-user
RBAC. See [docs/MCP.md](MCP.md). The same objects are also exposed as MCP
**resources** via one `nbox://{kind}/{ref}` template (the view `nbox_get`
returns). Per-user NetBox identity bridging ships as the credential vault
(`[serve.vault.<sub>]` maps each caller's OIDC `sub` to a per-user NetBox
token), and the MCP **prompts** catalog ships a curated set of read-only
investigation prompts (`ip_utilization_audit`, `cable_path_trace`,
`find_stale_prefixes`, `object_change_review`). A raw escape-hatch MCP tool is
later.

## Robustness

Retries HTTP 429 (`Retry-After` + backoff). `search` fails closed if an endpoint
errors (use `--partial` for best-effort). Targets NetBox 4.2+.
