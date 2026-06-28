---
name: nbox-search
description: Find NetBox objects across kinds in one query with nbox's ranked, deduped cross-object search — the entry point when the kind or exact reference isn't known yet. Use when the user wants to locate a device/IP/prefix/VLAN/site/etc. by a name fragment, or narrow by site/tenant/role/tag/status/VRF before a detail lookup.
---

# nbox search

`nbox search <query>` runs one parallel, full-text search across every object
kind — devices, sites, racks, rack-groups, IPs, prefixes, VLANs, circuits,
virtual-circuits, aggregates, ASNs, IP-ranges, tenants, contacts, providers,
VMs, VM-types, clusters, VRFs, route-targets — and returns one ranked, deduped
result set. It is the entry point when the exact kind or reference isn't known
yet: search first, then feed a hit into a detail lookup.

```bash
nbox --no-tui search edge01 --json --envelope
nbox --no-tui search edge --status active --site DC1 -o csv --cols kind,display,url
```

For the exact flags, run `nbox search --help`. This skill is flag-free by
design — it describes what each flag answers, not its semantics, so it can't
drift as the CLI evolves.

## What it answers

"What objects in NetBox match this string, and what kind is each?" Each hit
carries a `kind` (e.g. `device`, `ip_address`, `prefix`), a `display`, and a
`url`. Search is REST-canonical — NetBox's GraphQL has no full-text `q`, so a
`search = "graphql"` profile preference transparently falls back to REST.

## Narrowing the result set

- **One scope filter at a time.** The scope flags — `--site` / `--region` /
  `--site-group` / `--location` / `--tenant` / `--role` / `--tag` / `--status`
  / `--owner` / `--owner-group` / `--vrf` — narrow the search per-endpoint. The
  geographic scopes (`--site`/`--region`/`--site-group`/`--location`) are
  mutually exclusive: NetBox's polymorphic prefix `scope` is a single type+id,
  so passing more than one geographic scope is a usage error (exit 2). `--vrf`
  is orthogonal and may combine with a geographic scope.
- **Endpoints that can't honor a filter are skipped**, not errored — a VRF
  filter drops devices/sites/VLANs; a `--site` filter drops VRFs. The remaining
  kinds still return.
- **An unknown reference is a not-found error** (exit 4), not a silent empty
  result — a typo'd `--site` or `--vrf` is caught.
- `--limit` caps the result count; `--cols` selects the columns for `-o csv`
  output.

## Fail-closed by default

`search` fails closed: if any endpoint errors, the whole search exits non-zero
rather than return partial results. Pass `--partial` for best-effort results —
the kinds that succeeded are returned and the failed endpoints are reported on
stderr. The default is the safe one (no silently-missing kinds); reach for
`--partial` only when partial coverage is acceptable.

## Chaining into a detail lookup

A search hit's `kind` feeds straight into a detail lookup — that's the whole
point of searching first:

```bash
# 1. search to find the reference + its kind
nbox --no-tui search edge01 --json --fields kind,display,url

# 2. a hit with kind=device → nbox device; kind=ip_address → nbox ip
nbox --no-tui device edge01 --json
nbox --no-tui ip 203.0.113.10 --json
```

The detail commands (`nbox <kind> <ref>`) and `nbox get` accept the search
`kind` as-is (`ip_address` is an alias for `ip`). For per-kind detail, see the
[IPAM read](../ipam-read/SKILL.md) and [device context](../device-context/SKILL.md)
skills.

## Token trimming

`--fields a,b,c` keeps only those top-level fields per hit — pair it with
`--raw` to minimize tokens when the result set is large and only the reference
+ kind are needed for the next step.

## Reference

- [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md) — the
  complete command + flag reference, exit codes, and output flags
- [IPAM read](../ipam-read/SKILL.md), [Device context](../device-context/SKILL.md)
  — the detail lookups a search hit chains into
