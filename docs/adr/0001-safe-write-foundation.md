# ADR-0001: Safe Write Foundation

**Status:** Proposed
**Date:** 2026-06-26

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
   - an opaque confirmation token derived from the target, precondition, patch,
     profile, and plan expiry

   Domain view models are not write payloads. Write code gets explicit
   command-specific intent DTOs and endpoint-specific patch DTOs, then derives
   a minimal patch from the live NetBox object. If a field is not writable or
   cannot be shaped safely, the planner fails closed before any network write.

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
