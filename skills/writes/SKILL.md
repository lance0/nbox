---
name: nbox-writes
description: How to safely modify NetBox through nbox's write commands — the universal dry-run / confirm / audit lifecycle shared by all seven safe-write commands (interface description, device status, IP/prefix/ip-range reserve, tag add/remove). Use when the user wants to change NetBox objects, reserve IPs, or manage tags.
---

# nbox safe writes

`nbox` can modify NetBox through seven **safe-write** commands, all on the same
plan-first lifecycle (ADR-0001). Reads stay the default — a write never happens
without explicit opt-in.

## The universal lifecycle (all seven commands)

Every write follows the same path, so an agent can learn it once:

1. **Dry-run** (`--dry-run`) — builds a `MutationPlan`, shows the before/after
   diff, performs no mutation. Needs neither `--allow-writes` nor `--confirm`.
   With `--json`, returns the stable `MutationPlan` JSON (schema_version 1).

2. **Apply** (`--allow-writes --confirm`) — builds the same plan, checks the
   precondition (ETag/If-Match on 4.6+, last_updated on pre-4.6), and applies
   it. With `--json`, returns a `MutationReceipt` (schema_version 1).

3. **Refusal** — `--confirm` without `--allow-writes` is a usage error (exit 2,
   empty stdout). Non-TTY / `--json` / `--no-tui` never prompts.

## The seven write commands

| Command | Operation | What it does |
|---|---|---|
| `nbox interface <device> <iface> set description "…"` | update (PATCH) | Set an interface's description |
| `nbox device <name> set status <value>` | update (PATCH) | Set a device's status (validated live) |
| `nbox ip reserve <prefix>` | allocate (POST) | Reserve the next available IP |
| `nbox prefix reserve <cidr>` | allocate (POST) | Reserve the next available child prefix |
| `nbox ip-range reserve <start\|id>` | allocate (POST) | Reserve the next available IP from an IP range |
| `nbox tag add <type> <name> <tag>` | update (PATCH) | Add a tag to any object |
| `nbox tag remove <type> <name> <tag>` | update (PATCH) | Remove a tag from any object |

For the exact flags of each command, run `nbox <cmd> --help`. This skill is
flag-free by design — it points at `--help` so it can't silently drift as the
CLI evolves.

## Agent usage pattern

The recommended agent pattern is a two-step dry-run-then-apply:

```bash
# 1. Preview the plan (no mutation, no gate needed)
nbox --no-tui <cmd> ... --dry-run --json

# 2. If the plan looks correct, apply it
nbox --no-tui <cmd> ... --allow-writes --confirm --json
```

Between the two steps, inspect the `MutationPlan` JSON — it carries the target,
the field changes (before/after), the precondition, and any warnings. The
`MutationReceipt` from step 2 carries the outcome, the HTTP status, and (for
allocate writes) the created object.

## What the audit logs (and doesn't)

Every planned write emits one structured audit event (target `nbox::write_audit`)
with: the surface (cli/tui/mcp), the profile, the NetBox host, the operation, the
target kind/id, the **field names** that changed (never the values), the outcome,
the HTTP method/path, the status, and the latency. An opt-in `--message` is
recorded as a `message_present` flag + length — **never the message body**. The
token never appears in the audit log.

## The MCP server stays read-only

No write tools are exposed over MCP. Per-user NetBox write identity (Pattern 2
vault) is a prerequisite before MCP writes can ship. See ADR-0001 Decision 7.

## Reference

- [ADR-0001](https://github.com/lance0/nbox/blob/master/docs/adr/0001-safe-write-foundation.md) — the full safe-write foundation design
- [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md) — the complete command + flag reference
