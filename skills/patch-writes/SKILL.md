---
name: nbox-patch-writes
description: Update NetBox object fields using nbox's PATCH write commands — interface set description and device set status. Use when the user wants to change an interface description or a device's status in NetBox.
---

# nbox PATCH writes

Two **update** write commands modify specific fields on NetBox objects via
minimal `PATCH` bodies. Both follow the same dry-run / confirm / audit lifecycle
(see the [safe writes skill](../writes/SKILL.md)).

## The two PATCH commands

### `nbox interface <device> <interface> set description "…"`

Set an interface's description.

```bash
nbox --no-tui interface edge01 xe-0/0/1 set description "uplink to core" --dry-run --json
nbox --no-tui interface edge01 xe-0/0/1 set description "uplink to core" --allow-writes --confirm --json
```

Optional: `--message`. Run `nbox interface <device> <interface> set description
--help` for the full flag set.

### `nbox device <name> set status <value>`

Set a device's status. The status value is validated live against NetBox's
`OPTIONS` choices before any `PATCH` — a label (e.g. "active") is accepted
case-insensitively when it maps unambiguously to one canonical value; an
unknown or ambiguous status is a usage error (exit 2) naming the input and
listing the allowed values, before any `PATCH`.

```bash
nbox --no-tui device edge01 set status active --dry-run --json
nbox --no-tui device edge01 set status active --allow-writes --confirm --json
```

Optional: `--message`. Run `nbox device <name> set status --help` for the full
flag set.

## How they work

- **Minimal PATCH.** The plan carries only the field being changed —
  `{"description": "…"}` or `{"status": "active"}`. No other fields are sent.
- **A no-op** (current value already matches) sends no `PATCH` — the receipt
  reports `no_op: true`.
- **Concurrency.** ETag + If-Match on NetBox 4.6+; `last_updated` + before-hash
  on pre-4.6. A concurrent writer is caught (412 / hash mismatch) and the write
  is refused with a "re-run dry-run" message (exit 1).
- **Choice validation** (device status only): the allowed values are
  enumerated live from NetBox via a read-only `OPTIONS` request, so nbox
  never sends an invalid status. The normalization mechanism
  (`src/netbox/choices.rs`) is reusable for future REST choice fields.

## Reference

- [Safe writes skill](../writes/SKILL.md) — the universal lifecycle
- [ADR-0001](https://github.com/lance0/nbox/blob/master/docs/adr/0001-safe-write-foundation.md) — foundation design
