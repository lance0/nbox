# ADR-0001: Safe Write Foundation

**Status:** Accepted
**Date:** 2026-06-26
**Implemented:** 2026-06-26 by the safe-write foundation PR (`nbox interface <device> <iface> set description "…"`).

## Context

nbox is intentionally read-only today. That has been the right default for a
tool used from shells, SSH sessions, TUI workflows, and MCP hosts: every command
has a stable output contract, stdout stays data-only, logs and diagnostics stay
on stderr, and `nbox serve` exposes only read-only tools.

Writes are still worth adding, but only if they arrive as a small, shared
foundation rather than per-command ad hoc `PATCH` calls. A write path has to be
safe for humans and agents, readable in review, compatible with NetBox's
validation and audit model, and narrow enough that the first release cannot
become a generic object editor by accident.

The prior art is useful, but not directly portable:

- Junos uses a candidate configuration, `show | compare`, `commit check`,
  `commit confirmed`, automatic rollback, and explicit confirmation. That model
  is the right operator experience: inspect the delta, apply deliberately, and
  keep a visible safety boundary.
- NAPALM exposes a compact lifecycle around candidate load, diff, commit,
  confirmed commit, discard, and rollback. That API shape is a good reminder
  that automation needs the same steps as humans, just represented as data.
- NetBox is not Junos. NetBox REST writes are immediate object mutations, not a
  separate candidate datastore with a native rollback timer. nbox can require
  dry-run and confirmation, can use optimistic concurrency, and can read back
  NetBox change logs, but it must not claim v1 "commit confirmed" rollback
  semantics that NetBox does not provide.

Current nbox constraints also matter:

- REST is canonical. GraphQL is a read-side accelerator for selected views and
  is not the mutation path.
- The CLI output layer is centralized through `output::emit`; machine-readable
  stdout is already a compatibility surface.
- `nbox raw` is GET-only, deliberately reserving write verbs for a safe engine.
- MCP HTTP OIDC mode is read-only Pattern 3 today: caller attribution is local
  to nbox, while NetBox still sees one profile token. That is acceptable for
  reads, but not enough for per-user write authorization.
- `nbox history --diff` already exposes NetBox object-change before/after data
  and request IDs. Writes should use that as a receipt path instead of inventing
  a separate audit format.

## Decision

1. **Use one shared mutation model for every write.** Every mutating feature
   must build a `MutationPlan` first. The plan is the common contract for CLI,
   TUI, and future MCP writes. It contains:

   - `schema_version`
   - operation kind (`update` first; create/delete/allocate later)
   - target identity (kind, user reference, resolved id, display label, REST
     endpoint, profile)
   - the current server precondition (`ETag` when available, `last_updated` and
     a normalized before-hash otherwise)
   - normalized before/after field values for the fields in scope
   - the minimal REST patch body
   - a redacted, stable field diff
   - warnings, capability notes, and validation errors
   - optional `changelog_message`
  - an opaque confirmation token derived from the target, operation,
    precondition, patch, changelog message, profile, and plan expiry

   Domain view models are not write payloads. Write code gets explicit
   command-specific intent DTOs and endpoint-specific patch DTOs, then derives
   a minimal patch from the live NetBox object. If a field is not writable or
   cannot be shaped safely, the planner fails closed before any network write.
   The foundation work includes deserializing `last_updated` on the relevant
   REST wire objects for planner preconditions; current read view models do not
   need to expose it.

2. **REST `PATCH` is the v1 write transport.** NetBox documents `PATCH` as the
   partial-update verb and `PUT` as requiring a complete representation. nbox
   should generate minimal `PATCH` bodies against object detail endpoints. `PUT`
   remains out of scope until a command explicitly needs full-object replace
   semantics.

   NetBox remains the authoritative validator. nbox can preflight common
   mistakes for better UX, but API validation errors must be surfaced cleanly
   with field context and no stdout pollution. `OPTIONS` and the live OpenAPI
   schema can be used to discover writable fields, required fields, and choice
   values where hardcoding would become version-sensitive.

3. **Use optimistic concurrency whenever NetBox exposes it.** On NetBox 4.6+,
   object detail responses include `ETag` and writes can send `If-Match`; a stale
   object produces `412 Precondition Failed`. nbox should include `If-Match` on
   apply whenever the plan has an ETag. Older NetBox releases fall back to a
   read-before-write check using `last_updated` plus a normalized before-hash.
   If the object changed between plan and apply, nbox refuses the write and asks
   the caller to re-plan.

4. **Keep write enablement separate from confirmation.** `--confirm` means
   "apply the reviewed plan"; it must not silently turn a read-only nbox
   invocation into a write-capable one. Applying a plan requires both:

   - a local write-enable gate, initially an explicit CLI flag such as
     `--allow-writes` and later optionally a write-enabled profile setting; and
   - confirmation of the specific plan, either by interactive prompt or
     `--confirm`.

   Dry-run planning does not require the write-enable gate because it performs
   no mutation. NetBox token permissions remain authoritative: if the token is
   read-only or lacks object permission, the server rejection is surfaced as the
   final answer. The existing `ui.confirm_writes` setting is a future TUI
   preference, not a write-enable control.

5. **Make dry-run and confirmation explicit.** A mutating command always goes
   through the same lifecycle:

   1. Resolve the target unambiguously.
   2. Fetch the current object from REST.
   3. Build and validate a `MutationPlan`.
   4. Render the diff.
   5. Apply only after explicit confirmation.
   6. Re-fetch after success and emit a receipt.
   7. Clear or invalidate affected read-cache entries.

   CLI semantics:

   - `--dry-run` prints the plan/diff and performs no mutation, and needs neither
     the write-enable gate nor confirmation.
   - `--confirm`, together with the write-enable gate (Decision 4), applies
     without an interactive prompt, but only after building the same plan and
     checking its precondition. `--confirm` reviews and applies a plan; it does
     not enable writes. Confirmation without the write-enable gate is a usage
     error (exit `2`, empty stdout, stderr naming `--allow-writes`).
   - In plain output on a TTY, no `--dry-run` and no `--confirm` may prompt on
     stderr after showing the diff. The prompt must be an explicit positive
     confirmation, not a default-yes flow.
   - In non-TTY contexts, JSON output, CSV output, and `--no-tui`, nbox must not
     prompt. Without `--dry-run` or `--confirm`, it exits with usage code `2`,
     empty stdout, and stderr that names the required flag.
   - `--json --dry-run` returns the stable `MutationPlan` JSON. `--json
     --confirm` returns a stable `MutationReceipt` JSON.

   The confirmation token is not an authorization credential. It is only a guard
   that the caller is applying the same scoped plan it reviewed.

6. **Do not ship broad arbitrary object editing in v1.** The first write
   commands must be operation-specific and field-specific, for example a device
   status update, an interface description update, tag add/remove, or an IPAM
   allocation command. No `nbox edit <kind>`, no free-form JSON patch command,
   and no `nbox raw POST|PATCH|DELETE` should ship until they are built on the
   same planner, diff, confirmation, concurrency, and audit contracts.

7. **Keep MCP read-only until write identity is real.** Existing MCP tools stay
   read-only. Future write-capable MCP tools must:

   - be operation-specific, not a generic raw mutation tool;
   - require `nbox:write` and must not use `read_only_hint`;
   - return a plan/diff first and require an explicit apply call carrying the
     confirmation token;
   - use the same mutation engine as the CLI;
   - require Pattern 2 per-user NetBox credential bridging before writes are
     exposed over network-reachable MCP. Pattern 3 can attribute a caller in the
     nbox audit log, but NetBox would still see the shared service token, which
     is not acceptable for writes.

8. **Use clear audit and status wording.** Write logs and user-visible messages
   should describe facts, not magic:

   - dry-run: "planned, no changes sent"
   - no-op: "no change: current value already matches"
   - prompt refusal: "not applied"
   - stale precondition: "not applied: object changed in NetBox; re-run dry-run"
   - API validation failure: "not applied: NetBox rejected the patch"
   - success: "applied: <kind> <display> (<fields>)"

   Local write audit events go through `tracing` like the existing MCP audit
   path and never stdout. The event fields are an allow-list: surface
   (`cli`/`tui`/`mcp`), profile, NetBox host, operation, target kind/id/display,
   changed field names, dry-run/apply outcome, HTTP method/path, status, latency,
   and NetBox object-change request ID when a receipt lookup finds one. Do not
   log tokens, authorization headers, raw patch values, full objects, or the
   free-form changelog message body. A `message_present` flag and length are
   enough locally.

   NetBox `changelog_message` is opt-in via a write flag such as `--message`.
   nbox validates NetBox's 200-character limit before applying. If absent, nbox
   does not fabricate a message in NetBox; NetBox's own object-change record
   still records who, when, fields changed, and request ID.

9. **Test the contracts at the process boundary.** The write foundation must
   land with tests that preserve the current automation guarantees:

   - planner unit tests for minimal patches, no-op detection, redaction,
     choice/relation shaping, and unsupported-field failure
   - wiremock tests proving `--dry-run` performs no write request
   - wiremock tests proving `--confirm` sends the expected `PATCH`, `If-Match`
     when present, and `changelog_message` when provided
   - stale-precondition tests for `412` and the pre-4.6 fallback check
   - binary stdout/stderr tests for dry-run JSON, receipt JSON, validation
     errors, stale-object errors, usage failures, and prompt refusal
   - audit-log redaction tests proving values, tokens, and messages do not leak
   - MCP tests, when MCP writes exist, proving the JSON-RPC stream on stdout is
     never contaminated and write tools are not advertised as read-only
   - compatibility-doc updates for write-only version behavior, including the
     NetBox 4.6 `ETag`/`If-Match` row when the write engine lands

10. **Defer confirmed rollback.** Junos-style commit-confirmed semantics remain
   a future design, not v1. A safe NetBox implementation would need reversible
   inverse plans, dependency checks, durable pending state, conflict handling if
   another writer changes the object during the timer, and a clear story for
   creates/deletes/bulk operations. Until that exists, nbox should offer
   "plan -> confirm -> apply -> receipt", not "apply now and maybe auto-undo".

## Consequences

- The first write command will take longer to build than a direct `PATCH`, but
  the second write command should reuse most of the safety machinery.
- Operator and agent behavior stays legible: every mutation has a visible plan,
  an explicit confirmation boundary, and a receipt.
- NetBox remains the source of truth for validation, permissions, and object
  change history. nbox adds guardrails and better diffs around that API rather
  than replacing it.
- Read-only remains the default for CLI, TUI, MCP, and raw API access.
- Some attractive features are intentionally deferred: generic editing, raw
  write verbs, GraphQL mutations, write-capable MCP over Pattern 3, broad bulk
  writes, deletes, TUI edit mode, and commit-confirmed rollback.
- Scripts get one more stable JSON surface (`MutationPlan`/`MutationReceipt`),
  so schema versioning and golden tests need to cover it from the first write PR.
- NetBox 4.6+ gets stronger race protection through `If-Match`; NetBox 4.2-4.5
  remains supported with a conservative re-read check.

## Implementation status

Shipped on this foundation, in order:

- `interface <device> <iface> set description "…"` — the first write (`update` /
  `PATCH`).
- `device <device> set status <value>` — second write (`update` / `PATCH`), with
  the status value validated live against NetBox `OPTIONS` choices before any
  `PATCH`.
- `ip reserve <prefix> [--vrf] [--description] [--dns-name]` — the first
  **`allocate`** write: a `POST` to `…/prefixes/{id}/available-ips/` that reserves
  the next free address. It proves the foundation generalizes past `update`:

  - **`Operation::Allocate` drives a `POST`.** The operation kind selects the
    HTTP verb (`update` → `PATCH`, `allocate` → `POST`) and the audited
    `http_method`. Decision 2's "`PATCH` is the v1 write transport" is for
    in-place edits; a server allocation endpoint is a `POST` by NetBox's own
    design, not arbitrary creation.
  - **`Precondition::None`.** The allocation endpoint is server-side race-safe —
    NetBox never hands out the same address twice — and there is no prior object,
    `ETag`, or `last_updated` to bind, so an allocate carries no client
    precondition. It still folds into the confirmation token distinctly, so an
    allocate plan's token cannot collide with an update plan's. (Decision 3
    applies to in-place edits, where a concurrent writer is the hazard.)
  - **The receipt carries the created object.** `MutationReceipt` gained an
    optional `object` (the reserved IP's view) so scripts get the assigned
    address/id/status without a follow-up read. It is additive and omitted for
    `update`, so existing receipts are byte-identical and `schema_version` stays
    `1`. The dry-run shows the *currently* next address as an advisory warning,
    never as a guaranteed field — the applied address may differ.
- `tag add <type> <name> <tag>` — the fourth write (`update` / `PATCH`), the
  first **list-valued** field and the first write on **any object kind**. Tags
  are a list: the plan carries the full replacement `{"tags": [slugs]}` (NetBox
  `PATCH` replaces the whole array), so the before/after diff shows the tag slugs.
  The planner reads the object as a raw `serde_json::Value` — every NetBox
  object carries the same `tags` array shape — so no per-kind model is needed
  for this write. Adding a tag the object already carries is a no-op (empty
  patch, no `PATCH`). `ETag`+`If-Match` on 4.6+, `last_updated`+before-hash on
  pre-4.6, same as the interface/device pilots.
- `tag remove <type> <name> <tag>` — the fifth write (`update` / `PATCH`), the
  inverse of `tag add`. Shares one planner/applier with `tag add`
  (`TagOperation::Add`/`Remove`), proving the foundation extends to the inverse
  operation without new machinery. A no-op (tag already absent) produces an empty
  patch, no `PATCH`. Same `ETag`+`If-Match` / `last_updated`+before-hash
  concurrency contracts.
- `prefix reserve <cidr> [--vrf] [--length N] [--description]` — the sixth
  write (`allocate` / `POST`), the second `allocate`: a `POST` to
  `…/prefixes/{id}/available-prefixes/` that reserves the next free child block.
  It proves the `allocate` pattern extends past `ip reserve` with no new
  machinery — same `Operation::Allocate`, same `Precondition::None`, same
  gate/confirm/audit lifecycle, and the same `object`-in-receipt pattern. The
  body carries optional `prefix_length` (the desired child block size) and
  `description` (the v1 allow-list — no status/role/tags/vlan). The dry-run
  surfaces the currently-next block as an advisory warning; an exhausted parent
  (`409`) is a clean error.

- `ip-range reserve <start|id> [--description] [--dns-name]` — the seventh
  write (`allocate` / `POST`), the third `allocate`: a `POST` to
  `…/ip-ranges/{id}/available-ips/` that reserves the next free address within
  an IP range. It proves the `allocate` pattern extends to a third endpoint
  shape with no new machinery — same `Operation::Allocate`, same
  `Precondition::None`, same gate/confirm/audit lifecycle, and the same
  `object`-in-receipt pattern. The body carries optional `description` and
  `dns_name` (the same v1 allow-list as `ip reserve`). The dry-run surfaces the
  currently-next address as an advisory warning; an exhausted range (`409`) is
  a clean error.

- **Multi-IP allocation (`--count N`).** `ip reserve` and `ip-range reserve`
  accept `--count N` (default 1) to reserve N IP addresses in one invocation.
  The v1 implementation issues each IP as a separate `POST` (one per request);
  the receipt carries a JSON array of the created `IpView`s in `object`. The `count`
  is bound into the confirmation token (so a `count=3` plan cannot be replayed
  as `count=5`) and appears in the plan's fields diff when > 1.
  - **Partial failure.** If the k-th `POST` fails (k > 0), the receipt is
    returned with `partial: true`, `created_count: k`, and the k created IPs in
    `object`, but the process exits 1 (the audit logs `outcome=partial`) so
    scripts detect the incomplete allocation. A first-POST failure (k=0) is the
    existing single-reserve error path (exit 1, empty stdout). This is an
    `allocate`-specific outcome — `update` writes remain atomic single-`PATCH`.
  - **Backward compatible.** `count=1` (the default) is byte-identical to the
    existing single-IP plan/receipt: `count`, `partial`, `requested_count`, and
    `created_count` use `skip_serializing_if` defaults so they're omitted when
    at their default values. `schema_version` stays `1`.

Still deferred per Decision 6: choosing a specific address/block, interface/VM
assignment, status/tags on reserve, generic create/delete, and `nbox raw` write
verbs.

## References

- [NetBox REST API overview](https://netboxlabs.com/docs/netbox/integrations/rest-api/)
  for CRUD verbs, `PATCH`, `OPTIONS`, bulk behavior, and `changelog_message`.
- [NetBox 4.6 release notes](https://netbox.readthedocs.io/en/stable/release-notes/version-4.6/)
  for REST `ETag` and `If-Match` support.
- [NetBox change logging](https://netboxlabs.com/docs/netbox/features/change-logging/)
  for before/after object-change records, request IDs, and user messages.
- [Junos commit documentation](https://www.juniper.net/documentation/us/en/software/junos/cli/topics/topic-map/junos-configuration-commit.html)
  for candidate, commit check, commit confirmed, and rollback behavior.
- [Junos configuration comparison](https://www.juniper.net/documentation/us/en/software/junos/junos-xml-protocol/topics/task/junos-xml-protocol-requesting-configuration-comparison.html)
  for reviewable candidate/active diffs.
- [NAPALM changing configuration](https://napalm.readthedocs.io/en/latest/tutorials/changing_the_config.html)
  and [NAPALM NetworkDriver](https://napalm.readthedocs.io/en/latest/base.html)
  for a compact automation-facing plan/compare/commit/confirm lifecycle.
- [RFC 6241](https://datatracker.ietf.org/doc/html/rfc6241) for the NETCONF
  candidate, validate, commit, confirmed commit, discard, and locking model that
  informs the state-machine vocabulary.
