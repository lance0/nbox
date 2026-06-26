---
name: nbox-ipam-allocate
description: Reserve the next available IP or prefix in NetBox using nbox's three allocate write commands — ip reserve, prefix reserve, and ip-range reserve. Use when the user wants to allocate IP addresses or child prefixes from NetBox.
---

# nbox IPAM allocation

Three **allocate** write commands reserve the next available IP or prefix from
NetBox, server-side and race-safe. All share the same dry-run / confirm /
audit lifecycle (see the [safe writes skill](../writes/SKILL.md)).

## The three allocate commands

### `nbox ip reserve <prefix>`

Reserve the next available IP address from a prefix.

```bash
nbox --no-tui ip reserve 10.0.0.0/24 --dry-run --json        # preview
nbox --no-tui ip reserve 10.0.0.0/24 --allow-writes --confirm --json  # apply
```

Optional: `--vrf` (scope the prefix), `--description`, `--dns-name`,
`--message`. The receipt's `object` is the created IP. Run `nbox ip reserve
--help` for the full flag set.

### `nbox prefix reserve <cidr>`

Reserve the next available child prefix from a parent prefix.

```bash
nbox --no-tui prefix 10.0.0.0/24 reserve --dry-run --json
nbox --no-tui prefix 10.0.0.0/24 reserve --length 26 --allow-writes --confirm --json
```

Optional: `--length N` (child prefix size), `--vrf`, `--description`, `--message`.
The receipt's `object` is the created prefix. Run `nbox prefix reserve --help`
for the full flag set.

### `nbox ip-range reserve <start|id>`

Reserve the next available IP address from an IP range (not a prefix).

```bash
nbox --no-tui ip-range 10.0.0.10 reserve --dry-run --json
nbox --no-tui ip-range 10.0.0.10 reserve --allow-writes --confirm --json
```

Optional: `--description`, `--dns-name`, `--message`. The receipt's `object` is
the created IP. Run `nbox ip-range reserve --help` for the full flag set.

## Key properties

- **Server-allocated, race-safe.** NetBox picks the address/block and never
  hands out the same one twice. The plan carries no client precondition (no
  ETag / last_updated) — the POST is authoritative.
- **Dry-run advisory.** The dry-run shows the *currently* next available
  resource as an advisory warning. NetBox allocates at apply time, so the
  applied resource may differ (another client could allocate between preview
  and apply).
- **Receipt carries the created object.** The `--json` apply receipt's `object`
  field is the reserved IP or prefix view — scripts get the assigned resource
  without a follow-up read.
- **Exhaustion is a clean error.** A 409 (prefix/range exhausted) or 400
  (validation rejection) surfaces as exit 1, empty stdout, with the NetBox
  error message on stderr.

## Choosing a specific address

Choosing a *specific* address (not "next available") is deferred (ADR-0001
Decision 6) — it's a different operation (POST to `/ipam/ip-addresses/`, not
to `/available-ips/`) with different conflict semantics.

## Reference

- [Safe writes skill](../writes/SKILL.md) — the universal lifecycle
- [ADR-0001](https://github.com/lance0/nbox/blob/master/docs/adr/0001-safe-write-foundation.md) — foundation design
