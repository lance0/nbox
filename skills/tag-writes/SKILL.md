---
name: nbox-tag-writes
description: Add or remove tags on any NetBox object using nbox's tag write commands — tag add and tag remove. Use when the user wants to manage tags on devices, IPs, prefixes, VLANs, or any other NetBox object.
---

# nbox tag writes

Two **update** write commands manage tags on any taggable NetBox object,
sharing one planner/applier (`TagOperation::Add`/`Remove`). Both follow the
same dry-run / confirm / audit lifecycle (see the
[safe writes skill](../writes/SKILL.md)).

## The two tag commands

### `nbox tag add <type> <name> <tag>`

Add a tag to any taggable object.

```bash
nbox --no-tui tag add device edge01 prod --dry-run --json
nbox --no-tui tag add device edge01 prod --allow-writes --confirm --json
```

### `nbox tag remove <type> <name> <tag>`

Remove a tag from any taggable object.

```bash
nbox --no-tui tag remove device edge01 prod --dry-run --json
nbox --no-tui tag remove device edge01 prod --allow-writes --confirm --json
```

Optional: `--message`. Run `nbox tag add --help` / `nbox tag remove --help`
for the full flag set.

## How it works

- **`<type>`** is any read kind: device, ip, prefix, vlan, site, rack, circuit,
  vm, cluster, vrf, … — anything that carries a `tags` array.
- **`<name>`** is the object's reference (name, slug, address, CIDR, id — same
  resolver as `nbox <kind> <ref>`).
- **`<tag>`** resolves by id, exact name, or exact slug (same resolver as
  `nbox tagged`).
- NetBox `PATCH` **replaces the whole `tags` array**, so the plan carries the
  full replacement slug list. The before/after diff shows the tag slugs.
- A **no-op** (adding a tag the object already carries, or removing one it
  doesn't) sends no `PATCH` — the receipt reports `no_op: true`.
- Concurrency: ETag + If-Match on NetBox 4.6+; `last_updated` + before-hash on
  pre-4.6. A concurrent writer is caught and the write is refused with a
  "re-run dry-run" message (exit 1).

## Reference

- [Safe writes skill](../writes/SKILL.md) — the universal lifecycle
- [ADR-0001](https://github.com/lance0/nbox/blob/master/docs/adr/0001-safe-write-foundation.md) — foundation design
