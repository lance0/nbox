---
name: nbox
description: Query NetBox (DCIM/IPAM) from the shell with the `nbox` CLI — look up devices, interfaces, IPs, prefixes, VLANs, sites, racks, circuits, VRFs, tenants, VMs, and clusters; run cross-object search; and use IPAM helpers (next free IP/prefix, prefix utilization, cable-path traces). Use when the user asks about NetBox, network inventory, IP addressing, or device/interface/cable details, or wants machine-readable network data. Read-only; supports `-o json` / `-o csv` and an MCP server.
---

# nbox — NetBox from the shell

`nbox` is a fast, **read-only** CLI / TUI / MCP client for [NetBox](https://github.com/netbox-community/netbox)
(DCIM + IPAM). Use it to answer questions about network inventory and addressing
without clicking through the NetBox web UI. It never modifies NetBox.

## When to use this skill

Reach for `nbox` when the user wants to:

- look up a **device, interface, IP, prefix, VLAN, site, rack, circuit, provider,
  aggregate, ASN, IP range, tenant, contact, VM, cluster, VRF, or route target**;
- **search** NetBox across object types in one query (e.g. "find anything matching `edge01`");
- find a **free IP or prefix**, check **prefix utilization**, or **trace a cable path**;
- pull NetBox data as **JSON/CSV** for a script, audit, or report.

## Prerequisites

Assumes `nbox` is installed and pointed at a NetBox instance. Verify with `nbox
status` (reports connectivity + the NetBox version). If it fails with exit code `3`
(auth) or a missing-config error, the user still needs to install nbox and
configure a profile + token — see the
[README](https://github.com/lance0/nbox#readme).

## Core usage

Always pass a subcommand and request machine-readable output:

```bash
nbox --no-tui device edge01 -o json            # one device, full detail
nbox --no-tui interface edge01 swp25 -o json   # an interface + its cable path
nbox --no-tui ip 10.0.0.1 -o json              # an IP → most-specific parent prefix
nbox --no-tui prefix 10.0.0.0/24 -o json       # a prefix (children, utilization)
nbox --no-tui search edge01 -o json            # cross-object search (ranked, deduped)
nbox --no-tui next-ip 10.0.0.0/24 -o json      # next free address in a prefix
nbox --no-tui raw GET dcim/devices/?limit=1    # escape hatch (path with or without /api/)
```

Output flags:

- `--no-tui` — guarantee a non-interactive run (any invocation that would launch the
  TUI exits with a usage error instead of blocking on a terminal). **Always pass this.**
- `-o json` / `--json` — JSON to stdout; `--raw` for one compact line.
- `-o csv` — CSV for list/search results (single objects are rejected — use `--json`).
- `--fields a,b,c` — keep only those top-level fields; `--envelope` wraps as
  `{ "schema_version": 1, "data": … }` for stable parsing.

stdout carries only the requested data; logs/diagnostics/errors go to stderr. Exit
codes are stable: `3` auth, `4` not-found, `5` ambiguous. Full command + flag
reference: [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md).
