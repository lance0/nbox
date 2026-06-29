# ADR-0002: Local single-user MCP writes

**Status:** Proposed
**Date:** 2026-06-29
**Amends:** ADR-0001 §7 (MCP writes require per-user identity)

## Context

ADR-0001 §7 made MCP writes require a per-user OIDC identity: the caller's `sub`,
validated from an IdP-issued JWT over the HTTP transport, mapped through
`[serve.vault]` to that user's own NetBox token. The service profile token is
never used for writes; stdio and loopback static-bearer carry no `sub`, so they
are read-only.

That is the right model for a **shared, network-reachable** deployment, where a
write must be attributable to a real NetBox user. But it is currently the *only*
write path, and that is wrong for nbox's primary audience.

Most nbox users run it locally — on a laptop, one operator, an MCP host (Claude
Desktop / Claude Code) spawning `nbox serve` over **stdio**. For that user,
standing up an OIDC IdP just to let their agent reserve an IP is disproportionate
friction, and it is out of step with how MCP treats local servers:

- MCP's authorization model is OAuth 2.1 for **HTTP** transports; **stdio has no
  auth** — the host launched the subprocess as the user. Local MCP servers that
  mutate state (filesystem, git, sqlite, …) rely on the **host's per-tool
  approval UI** as the human gate, not on server-side identity.
- For a single local user, "attribution" is moot: there is one operator, and the
  profile token is already their token (reads use it).

So nbox's "no local writes" is a self-imposed restriction beyond the spec, and it
pushes a heavyweight requirement onto the common case. Today the gap is only
documented — local users are told to use the equivalent CLI command.

## Decision

Add an explicit, opt-in **single-user local write mode**, gated by a new, distinct
config key **`[serve].local_writes`** (with a `--local-writes` CLI flag). When
enabled, `nbox serve` accepts MCP writes that carry **no per-user identity** and
executes them under the **profile token** — the same token reads already use.

It is deliberately separate from the Pattern 2 path (`[serve].allow_writes` +
`[serve.vault]` + OIDC), which is unchanged. A write is authorized if **either**:

- **Pattern 2 (multi-user, unchanged):** an OIDC caller `sub` carrying the
  `nbox:write` scope with a `[serve.vault]` entry → that user's per-user token; or
- **Single-user local (new):** `[serve].local_writes = true`, the request carries
  **no** OIDC identity, and the transport is **stdio or a loopback HTTP bind** →
  the profile token.

The presence of an OIDC identity disambiguates the two: an authenticated request
always takes the Pattern 2 path (and still needs the scope + a vault entry);
`local_writes` is strictly the no-identity, local fallback. It never weakens the
multi-user path.

Hard safety rules:

1. **Loopback / stdio only.** `local_writes` never applies to a non-loopback
   bind. A routable HTTP server already *requires* OIDC; profile-token writes must
   never be reachable over the network. `--http <non-loopback>` with `local_writes`
   and no OIDC is a **startup usage error**.
2. **Explicit, off by default.** `local_writes` is its own key, never implied by
   `allow_writes`. An operator turns it on deliberately. There is no token/scope
   to check in this mode — the opt-in flag *is* the authorization, and the host's
   approval is the per-action gate.
3. **Unchanged everywhere else.** Same narrow operation set, same plan→apply
   two-step, the same **server-issued plan store** (a forged or edited plan is
   still rejected — `confirm_token` is not a secret MAC), the same write audit.
4. **Human-in-the-loop = the MCP host's approval.** Both write tools stay
   `read_only_hint = false`, so a host won't auto-run them; `nbox_apply_write` is
   the explicit apply call the host prompts the user to approve — the local
   analogue of the CLI's `--confirm` / TTY prompt.

This amends ADR-0001 §7: "the service token is never used for writes" becomes
"the service token is never used for writes **except** in the explicit,
loopback/stdio-only `local_writes` single-user mode."

## Consequences

Positive:

- The common case works: a local agent reserves an IP / sets a description with no
  IdP and no vault.
- Aligns with the MCP local trust model and with how other local MCP servers
  mutate state.
- Reuses the entire safe-write machinery (plan store, narrow surface, audit); the
  only new thing is the credential source.

Negative / trade-offs:

- `local_writes` writes ride the **profile token's** NetBox RBAC. Operators should
  scope that token to what the agent may change (the same defense-in-depth advice
  as for reads).
- No per-user attribution in this mode — acceptable by definition (one user). The
  write audit records `surface=mcp`, the profile, and the operation, with `sub`
  recorded as `local`.
- Two write-enable knobs (`allow_writes`, `local_writes`), kept distinct on purpose
  so the multi-user and single-user paths never blur.

Neutral:

- A non-loopback bind is unaffected (still OIDC-only).
- Not in 0.14.0 — ships as a focused fast-follow so it gets its own review.

## Implementation sketch

- `config.rs`: add `ServeConfig.local_writes: bool`. `cli.rs`: add `--local-writes`.
- Startup (`http.rs`): a non-loopback bind with `local_writes` and no OIDC is a
  usage error (exit 2), mirroring the existing non-loopback-needs-OIDC check.
- `write.rs::bridged_client`: when the caller is `None` (no OIDC identity),
  `local_writes` is enabled, and the transport is stdio / loopback, return the
  profile client (`self.client` clone) instead of rejecting. The `Some(caller)`
  (Pattern 2) path is unchanged. The plan store, scope-on-the-Pattern-2-path, and
  audit are untouched.
- Audit: record `sub = "local"` for the single-user path.
- Tests: `local_writes` enables a stdio write end to end through the plan store; a
  non-loopback bind + `local_writes` is a startup error; `local_writes` does not
  bypass the plan store (a forged plan is still rejected); the OIDC/Pattern 2 path
  is unchanged; with no `local_writes`, stdio writes still reject.

## Alternatives considered

- **Static-bearer → vault-of-one** (loopback HTTP maps `--http-token` to one
  configured identity): still needs the HTTP transport and more config; doesn't
  help the common stdio host.
- **OS-user as identity:** non-portable, surprising, a weak boundary.
- **Status quo + docs** (point local users at the CLI): leaves the real gap — an
  agent can't write over local MCP at all. Kept only as a fallback note, not the
  answer.

## References

- ADR-0001 §7 — the per-user-identity decision this amends.
- MCP authorization: OAuth 2.1 for HTTP transports; stdio is an unauthenticated,
  trusted-subprocess channel (the host mediates approval).
