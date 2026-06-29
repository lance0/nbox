---
name: nbox-serve
description: Run nbox as an MCP server (read-only by default; optional local stdio writes with --local-writes, or shared HTTP/OIDC writes with --allow-writes + nbox:write + [serve.vault]) so an agent host can query NetBox over JSON-RPC — stdio or HTTP+OIDC transport, the read tools (search/get/get_interface/history/…), the nbox://{kind}/{ref} resource template, the investigation prompts catalog, and --print-config for install recipes. Use when the user wants to wire NetBox into an MCP host or stand up the server.
---

# nbox serve (MCP server, read-only by default)

`nbox serve` exposes nbox's read layer as an MCP server. The tools reuse the
CLI's query + view layer, so they return the same JSON view models as the
equivalent `nbox <cmd>`. **Read-only is the default** — the write tools are always
listed but reject at call time unless writes are explicitly opted in (see below).
For the flags, run `nbox serve --help` — this skill is flag-free by design.

## Two transports

- **stdio (default).** An MCP host launches `nbox serve` as a subprocess and
  speaks JSON-RPC over stdin/stdout. JSON-RPC on stdout, logs on stderr;
  URL/token come from the active profile (same `--profile` / `--config` flags).
  Read-only by default; enable local single-user MCP writes with
  `nbox serve --local-writes` or `[serve].local_writes = true`, using the active
  profile token.
- **HTTP** — `nbox serve --http 127.0.0.1:8080`, same tools mounted at `/mcp`,
  loopback only with `Origin`/`Host` validation and an optional static bearer.
  Add `--oidc-issuer <URL>` + `--audience <VALUE>` for OAuth 2.1
  resource-server mode: inbound IdP JWTs are validated on `/mcp` (alg allowlist,
  `iss`/`aud`/`exp`, `nbox:read` scope), a routable bind is allowed (TLS
  terminates in front), and Protected Resource Metadata is served at
  `/.well-known/oauth-protected-resource`. HTTP `/mcp` also carries an audit log
  and an opt-in per-caller rate limit. This is **read-only Pattern 3**: the last
  hop to NetBox still uses the one local profile token, so the audit log is
  accountability, not per-user RBAC — trusted single-team read-only only.

## The read tools

Each maps to a CLI read and returns the same view model:

| Tool | What it answers |
| ---- | --------------- |
| `nbox_status` | Connection, capabilities, NetBox/Django/Python versions, token validity. Call first. |
| `nbox_search` | Cross-kind ranked search (one scope filter at a time). Find a reference, then `nbox_get`. |
| `nbox_get` | One object by `kind` + `ref` (`vrf`/`site`/`group` disambiguate). |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` / `nbox_next_prefix` | Next free address(es) / child block(s) — preview, reserves nothing. |
| `nbox_journal` | Operator journal entries for an object. |
| `nbox_history` | System audit log (create/update/delete, who + when); `diff=true` for full before/after. |
| `nbox_list_tags` / `nbox_tagged` | List tags; objects carrying a tag, across kinds. |
| `nbox_cache_clear` | Drop the local read cache (read-only w.r.t. NetBox). |

A search hit's `kind` (e.g. `ip_address`) feeds straight into `nbox_get`, which
accepts it as an alias. Every read tool that returns an object or hit list takes
an optional **`fields`** parameter — keep only those top-level keys to trim
tokens (unknown keys ignored).

## Resources and prompts

- **Resources.** The same objects are exposed via one template,
  `nbox://{kind}/{ref}` (e.g. `nbox://device/edge01`, `nbox://ip/203.0.113.10`),
  for hosts that browse/attach resources instead of calling tools. Reading one
  returns the same JSON as `nbox_get`; percent-encode a `ref` containing `/`
  (e.g. a CIDR). It's a template, so `resources/list` is empty.
- **Prompts.** A catalog of read-only investigation prompts ships with it —
  `ip_utilization_audit`, `cable_path_trace`, `find_stale_prefixes`,
  `object_change_review`. Each returns a structured plan naming the exact nbox
  tools to call, tailored to the supplied arguments — a plan, not data (no
  NetBox round-trip).

## Install recipe

`nbox serve --print-config` prints the paste-ready `mcpServers` JSON (absolute
binary path, echoed `--profile`/`--config`/`--local-writes`, placeholder token)
and exits — no server start, no NetBox connection. Drop it into the host's MCP
config.

```bash
nbox serve --print-config           # paste-ready mcpServers JSON, then exit
nbox serve                          # stdio, read-only
nbox serve --http 127.0.0.1:8080    # loopback HTTP, read-only
```

## Writes are a separate opt-in

The MCP server is read-only by default. The write tools (`nbox_plan_write` /
`nbox_apply_write`) are always registered, but a call only **executes** with
one explicit write mode: local stdio `nbox serve --local-writes`, which uses the
active profile token, or shared HTTP/OIDC `nbox serve --http --allow-writes` plus
the caller's `nbox:write` scope and a `[serve.vault]` entry mapping their OIDC
`sub` to a per-user NetBox token. HTTP/static-bearer profile-token writes reject
in this release.
`nbox_apply_write` applies the plan the server stored at plan time (looked up by
the `confirm_token` from `nbox_plan_write`), not the plan you resubmit. For that
lifecycle, see the [safe writes](../writes/SKILL.md) skill.

## Reference

- [docs/MCP.md](https://github.com/lance0/nbox/blob/master/docs/MCP.md) — the
  full MCP server design, per-host config paths, and auth details
- [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md) — the tool
  table and the CLI surface the tools reuse
- [Safe writes](../writes/SKILL.md) — the opt-in write tools and their gate
