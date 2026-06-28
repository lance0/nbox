# Comparison

nbox is a fast read path into NetBox, not a replacement for it. It answers the
questions you ask at the terminal — *what is this IP, where is this device, what
owns this prefix?* — from the shell, a TUI, or an MCP server, against the same
NetBox you already run. It does not write: reserve, allocate, and edit still
belong to the web UI or a Python client. The tables below compare nbox against
the alternatives a NetBox user already has — the NetBox web UI, raw REST over
`curl`, and `pynetbox` (the official Python client) — and say when to reach for
which.

## Capability matrix

✓ = first-class · ◐ = possible but manual · ✗ = not supported

| Task | nbox | NetBox web UI | curl / REST | pynetbox |
|------|:----:|:-------------:|:-----------:|:--------:|
| IP → prefix → VLAN → scope in one step | ✓ (one command) | ◐ (clicks across pages) | ◐ (several requests, manual joins) | ◐ (several calls, manual joins) |
| Cross-object-type search in one query | ✓ (ranked, deduped) | ✗ (per-type search pages) | ◐ (one request per endpoint) | ◐ (one call per endpoint) |
| Prefix utilization + navigable prefix tree | ✓ | ✓ (visual) | ◐ (compute from children) | ◐ (compute from children) |
| Next free IP / next free prefix | ✓ (`next-ip` / `next-prefix`, nothing reserved) | ◐ (available-IPs view) | ◐ (available endpoints) | ◐ (available methods) |
| Works over SSH with no runtime | ✓ (single static binary) | ✗ (needs a browser) | ◐ (only if curl present) | ✗ (needs Python + the package) |
| Machine output (JSON/CSV) + stable exit codes | ✓ (built in) | ✗ | ◐ (you build it) | ◐ (you build it) |
| Built-in AI-agent access (MCP) | ✓ (`nbox serve`) | ✗ | ✗ | ✗ |
| Reserve / allocate / edit (writes) | ✗ (read-only today) | ✓ | ✓ | ✓ |
| Cross-object navigation (device ↔ IP ↔ prefix ↔ VLAN) | ✓ (TUI `R`; resolved inline in CLI) | ✓ (links) | ✗ (you chase ids) | ✗ (you chase ids) |
| Learning curve | low (subcommands + flags) | low (point and click) | high (endpoints, filters, joins) | medium (object model, Python) |

Notes:

- nbox is **read-only** today — writes are deferred. For anything that mutates
  NetBox, use the web UI or pynetbox.
- `next-ip` / `next-prefix` query NetBox's available-IPs / available-prefixes
  endpoints (read-only; nothing is reserved in NetBox); `next-prefix --length L`
  picks the first fitting block locally with `ipnet`.
- nbox targets NetBox **4.2+**. REST is canonical; GraphQL is an opt-in
  accelerator for the VRF and route-target views.
- "Works over SSH with no runtime" is also nbox's agent edge: one static binary,
  zero runtime, MCP-ready in ~9 ms — versus an interpreter plus packages for a
  Python MCP server. Measured numbers and method in [BENCHMARKS.md](BENCHMARKS.md).

## When to use each

### nbox

- Fast single-object lookups from the shell — `device`, `ip`, `prefix`, `vlan`,
  and the rest, each a one-liner that resolves related objects inline.
- One query across object types — `search` runs in parallel over devices,
  sites, racks, IPs, prefixes, VLANs, circuits, aggregates, ASNs, IP ranges,
  tenants, contacts, providers, VMs, clusters, VRFs, and route targets, then
  returns ranked, deduped hits.
- Over SSH on a jump host — one static binary, no Python or browser to install.
- Scripting and CI — `--json`/`-o csv`, `--fields`, `--envelope`, and stable
  exit codes (`0` ok, `1` generic/API, `2` usage, `3` auth, `4` not found, `5`
  ambiguous); stdout stays clean, logs go to stderr.
- Feeding an AI agent — `nbox serve` is a read-only MCP server (nine tools,
  stdio or loopback HTTP with OIDC) returning the same JSON view models the CLI
  does.

### The NetBox web UI

- Writes — create, edit, delete, reserve, allocate.
- Admin and bulk edits — bulk import/edit, permissions, custom field
  definitions.
- Visual views — rack elevations, cable/topology views, change logs, and the
  full object graph with clickable relationships.
- One-off browsing when a terminal is not where you are.

### pynetbox

- Programmatic **writes** — create and update objects from Python.
- Large custom automations — provisioning workflows, reconciliation jobs, and
  anything that needs the full object model in a real programming language.
- Integration with existing Python codebases that already speak NetBox.

### Raw curl

- One-off calls to endpoints nbox does not model.
- nbox has a read-only escape hatch for this: `nbox raw GET <api-path>` issues
  the request with the active profile's auth and returns the JSON, so you stay
  in one tool for unmodeled **reads** (it is GET-only — for writes, use curl or
  pynetbox directly).

## Migrating common lookups

### Find a device's primary IP

```bash
# pynetbox
nb.dcim.devices.get(name="edge01").primary_ip4.address

# nbox
nbox device edge01 --json | jq -r '.primary_ip4'   # one call, JSON to jq
```

### What is this IP (address → prefix → VLAN → scope)

```bash
# curl: one request for the address, then more to walk up to its prefix/VLAN/scope
curl -s -H "Authorization: Token $TOKEN" \
  "$NB/api/ipam/ip-addresses/?address=10.44.208.55"        # then chase prefix, then VLAN, then scope

# nbox: resolved in one command
nbox ip 10.44.208.55                                       # address → parent prefix → VLAN → scope
```

Add `--vrf <name>` when the same address exists in several VRFs (otherwise a
duplicate exits `5` and lists the candidates).

### Search by name across object types

```bash
# curl: one request per endpoint, then merge the results yourself
curl -s -H "Authorization: Token $TOKEN" "$NB/api/dcim/devices/?q=edge01"   # repeat per endpoint

# nbox: one ranked, deduped query across every searchable type
nbox search edge01                                         # devices, IPs, prefixes, VLANs, …
nbox search edge01 -o csv --cols kind,display,url          # tabular, for a spreadsheet
```

### Next free /26 in a prefix

```bash
# pynetbox: query the available-prefixes endpoint, then pick a /26
nb.ipam.prefixes.get(prefix="10.0.0.0/8").available_prefixes.list()   # then filter for a /26

# nbox: read-only, nothing reserved
nbox next-prefix 10.0.0.0/8 --length 26                    # first free /26 (add --vrf to scope)
nbox next-ip 10.44.208.0/24 --count 4                      # or the next free addresses
```

---

See the [README](../README.md) for setup and the full command list, and
[docs/FEATURES.md](FEATURES.md) for the complete command reference.
