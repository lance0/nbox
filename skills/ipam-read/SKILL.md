---
name: nbox-ipam-read
description: Answer IP addressing questions from NetBox with nbox's IPAM read commands — look up an IP and its parent prefix/VLAN/scope, check prefix utilization and the prefix tree, preview the next free IP/prefix, and inspect VLANs, IP ranges, aggregates, VRFs, and route targets. Use when the user asks about addressing, allocation, or routing context.
---

# nbox IPAM reads

The IPAM read commands answer addressing and routing questions: where an
address lives, how full a prefix is, what's free, and how VRFs/route-targets
relate. All are read-only. For the flags of any one, run `nbox <cmd> --help` —
this skill is flag-free by design, describing what each command answers.

## What each command answers

- **`nbox ip <address>`** — one IP, enriched with its most-specific parent
  prefix (VRF-scoped), that prefix's VLAN, and the prefix's `scope`/`scope_type`
  (site, location, region, …). Surfaces `nat_inside`/`nat_outside` (NetBox 4.6)
  when set. "Where does `203.0.113.10` live, and in what prefix/VLAN/site?"
- **`nbox prefix <cidr>`** — one prefix with its utilization, child prefixes,
  and contained IPs. "How full is `10.0.0.0/24`, and what's under it?"
- **`nbox next-ip <cidr>`** / **`nbox next-prefix <cidr>`** — a read-only
  **preview** of the next free address / child block. They reserve **nothing**
  (see the warning below).
- **`nbox vlan <vid|name>`** — one VLAN with its referencing prefixes, its own
  `scope`/`scope_type`, and (when it belongs to a scoped VLAN group) the group's
  `group_scope`/`group_scope_type`.
- **`nbox ip-range <start|id>`** — one IP range (the start address or id).
- **`nbox aggregate <cidr|id>`** — one aggregate (the top-level allocation a
  prefix tree sits under).
- **`nbox vrf <name|rd|id>`** — a VRF as a routing context: RD, tenant,
  enforce-unique, import/export route targets, counts, plus its prefix tree and
  scoped addresses.
- **`nbox route-target <name|id>`** — a route target (e.g. `65000:100`) plus the
  VRFs that import and export it.

## next-ip / next-prefix reserve NOTHING

`next-ip` and `next-prefix` are **read-only previews** — they show what NetBox
*would* hand out next, but claim nothing and race with other clients. To
actually claim an address or block, use the `reserve` write commands, which POST
to NetBox's `available-ips` / `available-prefixes` endpoints under the dry-run /
confirm gate. See the [IPAM allocate](../ipam-allocate/SKILL.md) and
[safe writes](../writes/SKILL.md) skills.

```bash
nbox --no-tui next-ip 10.0.0.0/24 --json        # preview only — reserves nothing
nbox --no-tui ip 203.0.113.10 --json --fields address,parent_prefix,scope,assigned
nbox --no-tui prefix 10.0.0.0/24 --json         # utilization + children
```

## VRF disambiguation and the exit-5 ambiguity

A reference that matches more than one object across VRFs (the same address or
CIDR existing in several VRFs) exits **5** and lists the candidate VRFs rather
than guessing. Scope the lookup with `--vrf <name|rd|id>` to resolve it:

```bash
nbox --no-tui ip 10.0.0.1 --json                # exit 5 if 10.0.0.1 is in >1 VRF
nbox --no-tui ip 10.0.0.1 --vrf CUST-A --json   # disambiguated
```

`ip`, `prefix`, and `next-ip`/`next-prefix` all take `--vrf` (name, RD, or id —
VRFs have no slug). A VLAN that exists at several sites exits 5 the same way —
scope it with `--site` / `--group`.

## Reference

- [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md) — the
  complete command + flag reference, exit codes, and scope/VRF resolution
- [IPAM allocate](../ipam-allocate/SKILL.md) — the `reserve` write commands that
  actually claim an IP/prefix (vs. these read-only previews)
- [Device context](../device-context/SKILL.md) — to trace an IP's assigned
  interface and device
