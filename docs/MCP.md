# MCP server

`nbox serve` runs a read-only [MCP](https://modelcontextprotocol.io) (Model
Context Protocol) server over the stdio transport. An MCP host launches the
`nbox` binary as a subprocess and speaks JSON-RPC over its stdin/stdout; the
tools reuse the same NetBox query + view layer as the CLI, so they return the
same JSON view models. Nothing is ever written.

## Prerequisites

A configured profile, exactly as the CLI needs one: a NetBox `url` and a token.
`nbox serve` resolves the token the same way every other command does, in
precedence order: the env var named by the profile's `token_env`, then
`NBOX_TOKEN`, then the profile's `token`. It honors the same global flags
(`-p`/`--profile <name>`, `--config <path>`). See [docs/CONFIG.md](CONFIG.md) for
profiles and token resolution. Confirm the CLI works first:

```bash
nbox status
```

If that connects, the MCP server will too — it uses the same path.

Backend routing is identical to the CLI. `nbox_search` always uses REST.
`nbox_get vrf` and `nbox_get route_target` may use GraphQL for their
child/relation bundles when `nbox_status.api` reports an effective `graphql`
backend; identity/header resolution remains REST.

## Connecting it to a host

The host launches `nbox serve` and provides the NetBox token in the
subprocess's environment.

### Claude Code

Register a stdio server with the `claude mcp add` CLI. The `--` separates
`claude`'s flags from the command it will run; `-e` sets an env var on the
subprocess:

```bash
claude mcp add nbox -e NBOX_TOKEN=nbt_xxx.yyy -- nbox serve
```

Add `--profile <name>` after `serve` to pin a profile, or `--config <path>` to
point at an alternate config file. If `nbox` is not on the host's `PATH`, use
its absolute path in place of `nbox`.

### Generic host (Claude Desktop and others)

Most hosts read a JSON `mcpServers` object. Add an `nbox` entry that runs the
binary with `serve` and supplies the token in `env`:

```json
{
  "mcpServers": {
    "nbox": {
      "command": "nbox",
      "args": ["serve"],
      "env": {
        "NBOX_TOKEN": "nbt_xxx.yyy"
      }
    }
  }
}
```

Use an absolute path for `command` if `nbox` is not on the host's `PATH`. Add
profile/config flags to `args` if needed, e.g. `["serve", "--profile", "work"]`.
The exact file and the menu used to edit it differ per host — consult that
host's MCP documentation; the object shape above is what they consume.

### `nbox serve --print-config` (the recipe, generated)

Rather than hand-write the block, ask nbox for it — `--print-config` emits the
`mcpServers` JSON to stdout and exits, without starting the server or connecting
to NetBox (so it works before you've even set a token):

```bash
$ nbox serve --print-config
{
  "mcpServers": {
    "nbox": {
      "args": ["serve"],
      "command": "/usr/local/bin/nbox",
      "env": { "NBOX_TOKEN": "<set-your-token>" }
    }
  }
}
```

The `command` is the absolute path to this `nbox` binary (so the host finds it
even if `nbox` isn't on its `PATH`); `args` echoes any `--profile`/`--config`
you passed so the snippet reproduces your invocation; and `NBOX_TOKEN` is a
placeholder — set it there, export it in your shell, or drop the `env` block
entirely if `nbox config init` already holds the token. (This prints the stdio
recipe; the HTTP/OIDC transport is configured separately — see below.)

### Where each host reads it

| Host | File |
| ---- | ---- |
| **Claude Code** | `.mcp.json` in the project root (per-project) or `~/.claude.json` (global); `mcpServers` at the top level |
| **Claude Desktop** | `claude_desktop_config.json` — macOS: `~/Library/Application Support/Claude/`, Windows: `%APPDATA%\Claude\`, Linux: `~/.config/Claude/` |
| **Cursor** | `.cursor/mcp.json` in the project root |
| **Generic** | whatever the host documents; the `mcpServers` object above is the common shape |

Paste the printed object into the host's config file (merging under its
`mcpServers` key if the file already has other servers), restart the host, and
the `nbox` server appears in its MCP server list.

## HTTP transport (loopback)

Stdio is the default transport. For local clients that want HTTP framing
instead, serve over a loopback address — the HTTP transport ships in the default
build, no extra flags:

```bash
nbox serve --http 127.0.0.1:8080
```

(The transport lives behind the `http` cargo feature, which is on by default;
`cargo install nbox --no-default-features` gives a lean stdio-only build.)

The same eleven tools are mounted at `/mcp` (Streamable HTTP). It binds **only**
loopback: a non-loopback address (e.g. `0.0.0.0:8080`) is a usage error unless
the OIDC resource-server auth mode is configured (see below) — there is no other
bypass flag. The trust boundary is the loopback interface; the same profile/token
resolution and `-p`/`--config` flags apply.

Security on the HTTP path:

- DNS-rebinding defense via an allowed-host set. In loopback mode that set is
  loopback-only (`localhost`, `127.0.0.1`, `::1`). The `Host` header is validated
  against it on every request, and an `Origin` header — when the client sends one
  — must resolve to a host in the same set, else `403`. A request with no `Origin`
  (a non-browser client) is not rejected for the absent header; the loopback bind
  and Host check are its boundary. (See the OIDC section for how the set grows in
  routable mode.)
- `MCP-Protocol-Version: 2025-11-25` is advertised on every response — including
  the `401`/`403` auth challenges and the `429` rate-limit response.
- stdout stays clean (the protocol travels over the HTTP body); all logs go to
  stderr/file, exactly as in stdio mode.

Optional static bearer for the loopback endpoint — set a token and every request
to `/mcp` must carry `Authorization: Bearer <token>` (constant-time compared;
missing or wrong is `401`). It is never logged. Without one, loopback is the only
boundary.

```bash
# Flag, env var, or config — the flag wins, then the env var, then config.
nbox serve --http 127.0.0.1:8080 --http-token "$(openssl rand -hex 16)"
NBOX_SERVE_TOKEN=… nbox serve --http 127.0.0.1:8080
```

Or in the config file (prefer the env var over storing a secret here):

```toml
[serve]
http = "127.0.0.1:8080"
http_token = "…"   # optional
```

## OIDC resource-server auth (network-reachable)

For a network-reachable, multi-user deployment, run nbox as an OAuth 2.1
**resource server**: it validates inbound IdP JWTs on `/mcp` and advertises
Protected Resource Metadata. nbox does not mint tokens or run login — issuance
and the user interaction are the IdP's job. This works with any conformant OIDC
provider (Okta, Entra ID, Keycloak, Authentik, …); nbox is provider-agnostic.

```bash
nbox serve --http 0.0.0.0:8080 \
  --oidc-issuer https://idp.example.com \
  --audience https://nbox.example.com
```

`--oidc-issuer` enables the mode; `--audience` is then **required** — it is the
`aud` nbox expects, i.e. nbox's own canonical resource URI. With OIDC configured
the bind may be routable (the loopback restriction is lifted); **terminate TLS in
front** (a reverse proxy) — nbox serves plain HTTP and logs a warning on a
non-loopback bind. By default the JWKS URL is discovered from the issuer's
`/.well-known/openid-configuration` (falling back to
`/.well-known/oauth-authorization-server`); pass `--oidc-jwks-url` to set it
explicitly.

**HTTPS is required for the IdP.** The issuer, the JWKS URL (override or
discovered), and any discovered endpoint must use `https://` — a plain-`http://`
IdP URL lets a network attacker swap the signing keys and mint any token. The one
exception is a **loopback** host (`127.0.0.0/8`, `::1`, `localhost`), for local
development against a throwaway IdP. A plain-`http://` non-loopback IdP URL is a
startup error (exit `2`) and nbox never fetches keys over plaintext. The same rule
is re-applied to every HTTP **redirect** the IdP client follows, so an `https://`
endpoint can't `30x`-redirect the fetch down to a plain-`http://` non-loopback URL
— such a redirect fails the request rather than being followed.

**Allowed hosts (DNS-rebinding defense in routable mode).** Because the bind is
routable, the allowed-host set is widened from loopback-only to also include the
**host of `--audience`** (nbox's own identity) plus loopback. A real proxied
request whose `Host` (and `Origin`, when present) is that host passes; a
mismatched host is `403`. Add more accepted hosts — e.g. an alternate vhost in
front of the proxy — with `--allowed-host <HOST>` (repeatable) or
`[serve].allowed_hosts`; they are additive on top of the audience host and
loopback. (In loopback mode `--allowed-host` is ignored — the set stays
loopback-only.)

An entry (or the `--audience` host) with an **explicit port** matches only that
`host:port` — e.g. `nbox.example.com:8443` accepts that host on `8443` and
rejects it on any other port. An entry with **no port** matches the host on any
port (the default). Loopback always passes on any port. The same port rule
applies to both the `Host` check and the `Origin` check, so they agree. An entry
whose port is malformed — out of range (`host:99999`), non-numeric (`host:abc`),
or empty (`host:`) — is rejected at startup (`exit 2`, naming the entry) rather
than dropped to an any-port match, so a typo can't silently widen the allow-list.

What nbox validates on each `/mcp` request: the bearer from the `Authorization`
header (tokens in the query string are rejected); the JWT signature against the
issuer's JWKS, selected by `kid`, with an explicit algorithm allowlist
(RS256/ES256 — the token's own `alg` is never trusted, `none` is rejected); `iss`
exact-match; `aud` contains the configured audience; and `exp` in the future (with
a ≤120 s clock-skew leeway). The 10 read-only tools require the `nbox:read` scope.
JWKS is cached by `kid` (an unknown `kid` triggers a single rate-limited refresh,
then rejects; a transient JWKS outage keeps serving from the cache).

Failures use the standard challenges: a missing token → `401` with a bare
`WWW-Authenticate: Bearer resource_metadata="…"`; an invalid/expired token →
`401` with `… error="invalid_token"`; an authenticated request lacking the
scope → `403` with `WWW-Authenticate: Bearer error="insufficient_scope",
scope="nbox:read"`. The token is never logged or echoed in an error.

```toml
[serve]
http = "0.0.0.0:8080"
oidc_issuer = "https://idp.example.com"
audience = "https://nbox.example.com"
jwks_url = "https://idp.example.com/keys"   # optional override
allowed_hosts = ["nbox.example.com"]        # optional; audience host is allowed already
```

**#1 misconfiguration:** the IdP must be configured to mint the `aud` that
matches `--audience`, via the RFC 8707 `resource` parameter on the token request.
If it doesn't, the IdP returns a 200 with a token and nbox returns 401 — the
token is valid but its audience isn't nbox. The Protected Resource Metadata
endpoint (below) advertises the exact `resource` value clients should request.

### Protected Resource Metadata (RFC 9728)

`GET /.well-known/oauth-protected-resource` returns the resource-server
descriptor, **without** auth:

```json
{
  "resource": "https://nbox.example.com",
  "authorization_servers": ["https://idp.example.com"],
  "scopes_supported": ["nbox:read", "nbox:write"],
  "bearer_methods_supported": ["header"],
  "jwks_uri": "https://idp.example.com/keys"
}
```

### Accountability, not per-user RBAC (read-only Pattern 3)

This OIDC mode is **read-only Pattern 3** (DESIGN §24). nbox verifies the caller's
IdP token and attributes every request to that caller in the audit log — but the
last hop to NetBox still uses the **single local profile token** (a read-only
service credential). So NetBox itself sees *one* service identity, not the real
caller. That is **accountability, not authorization**: the audit log says who
asked, but NetBox's object permissions and changelog still attribute the call to
the service account. Every authenticated caller therefore gets the service
account's read rights — there is no per-user RBAC.

Run this mode only for a **trusted, read-only, ideally single-team** deployment.
Use a NetBox token scoped to exactly what an agent should see (read-only); that
token is the real privilege boundary. Multi-tenant use, writes, and real per-user
NetBox RBAC require per-user identity → NetBox-token bridging (a credential vault
keyed by the OIDC `sub`, so NetBox sees the real user) — the documented v2
(DESIGN §24, Pattern 2). The validated caller identity (`sub`, `client_id`,
`scope`, `jti`, `iss`) is already plumbed through for it.

## Operations (HTTP transport)

Two operational features apply to the HTTP `/mcp` endpoint (not the
`/.well-known/*` routes), in both loopback and OIDC modes.

### Audit log

Every authenticated request to `/mcp` emits **one** structured `tracing` event
under the target `nbox::audit`, recording:

- **WHO** — from the validated identity in OIDC mode: `sub`, `client_id`, `scope`,
  `jti`, `iss`. In loopback / static-bearer mode there is no token identity, so
  the event records the auth mode (`loopback` / `static-bearer`) and the peer IP.
  An `auth` field always names the mode; a `caller` field is the attributed key
  (`sub:` → `client:` → `ip:`).
- **WHAT** — the HTTP `method` and `path`. The JSON-RPC method / MCP tool name is
  **not** surfaced: extracting it would mean buffering the request body and would
  break the streaming transport, so the audit is request-level (method + path),
  which is honest and cheap.
- **WHEN / correlate** — a per-request `request_id`, plus `session` (a short
  SHA-256 prefix of the `Mcp-Session-Id`) when the client sends one. The session
  id is **hashed**, not logged raw — it stays correlatable across a session's
  requests without putting the raw session handle in the log.
- **OUTCOME** — the response `status`, a coarse `outcome`
  (`ok` / `auth-failed` / `rate-limited` / `error`), and `latency_ms`.

The token, the `Authorization` header, and any secret are **never** logged — the
fields are an explicit allow-list. Audit events follow the same sink discipline
as all logging (stderr or the configured `--log-file`, never stdout).

The events log at `info` under `nbox::audit`, so the default `warn` filter
**excludes** them. Opt in via `--log-level` / `NBOX_LOG` / `RUST_LOG`:

```bash
# Just the audit log:
NBOX_LOG="warn,nbox::audit=info" nbox serve --http 127.0.0.1:8080
# nbox at debug including audit, then silence audit specifically:
NBOX_LOG="nbox=debug,nbox::audit=off" nbox serve --http 127.0.0.1:8080
```

Pair it with `--log-file` for a durable, JSON-friendly record:

```bash
nbox serve --http 127.0.0.1:8080 --log-file /var/log/nbox-audit.log \
  --log-level "warn,nbox::audit=info"
```

### Per-caller rate limit

`--rate-limit <N>` (or `[serve].rate_limit`) caps requests per minute on `/mcp`.
The flag wins over the config; absent / `0` disables it entirely (the default —
existing behavior is unchanged unless you opt in).

When enabled it applies on two levels, both at `N`/minute:

- **Pre-auth, per peer IP.** Every request — including unauthenticated and
  invalid-bearer ones — is checked against a coarse per-peer-IP bucket *before*
  authentication. This throttles a flood of missing/invalid-token requests from a
  single peer (which would otherwise return `401`/`403` without ever reaching a
  limiter and could hammer JWT validation unthrottled). The check is per peer IP,
  so one peer flooding never throttles another.
- **Post-auth, per caller.** An authenticated request additionally honors a
  per-caller bucket keyed on the OIDC `sub` (else `client_id`). This catches a
  single identity spread across many source IPs.

A loopback / static-bearer caller has no token identity, so its peer-IP bucket
*is* its caller bucket — that one request is charged once, not twice. An OIDC
caller has a distinct per-`sub` bucket, so it honors both the coarse peer cap and
its own per-caller cap. Either limit being exceeded → `429 Too Many Requests`
with a `Retry-After` (seconds) header and the `MCP-Protocol-Version` header, and a
`rate-limited` audit event (the unauthenticated case is audited too, attributed to
the peer IP with no identity).

```bash
nbox serve --http 0.0.0.0:8080 \
  --oidc-issuer https://idp.example.com --audience https://nbox.example.com \
  --rate-limit 120
```

```toml
[serve]
http = "0.0.0.0:8080"
rate_limit = 120   # requests per caller per minute; 0/absent = disabled
```

## Tools

All tools are annotated read-only.

| Tool | Purpose |
| ---- | ------- |
| `nbox_status` | Connection target, per-surface `api` routing (configured vs effective backend), capabilities, NetBox/Django/Python versions, **and a token-validity preflight** (`token`: `valid`/`invalid`/`unverified` — the authenticated user on `valid`; NetBox 4.5+ `/api/authentication-check/`). Call first to confirm reachability, a valid token, and inspect the `api`/`capabilities` objects. |
| `nbox_search` | Free-text search across devices, sites, racks, rack groups, IPs, prefixes, VLANs, circuits, virtual circuits, aggregates, ASNs, IP ranges, tenants, contacts, providers, virtual machines, VM types, clusters, VRFs, and route targets. Optional `limit`, `status`, `site`, `region`, `site_group`, `location`, `tenant`, `role`, `tag`, `owner`/`owner_group` (4.5+; user/group by name), and `vrf` filters (`vrf` filters IP/prefix results only; only one scope filter at a time). Use it to find an object's exact reference. |
| `nbox_get` | Fetch one object by `kind` + `ref`. An ambiguous `ref` returns a candidate list; pass `vrf` (ip/prefix) or `site`/`group` (vlan) to disambiguate. |
| `nbox_get_interface` | One interface on a device: its config, assigned addresses, and cable-path trace. |
| `nbox_next_ip` | Next available address(es) within a prefix. `count`, `vrf`. Nothing is reserved. |
| `nbox_next_prefix` | Available child prefix(es) within a prefix. `length` returns the first free block of that size, else all free blocks. `vrf`. Nothing is reserved. |
| `nbox_journal` | Recent journal entries for an object, newest first. `kind`/`ref` as `nbox_get`. |
| `nbox_history` | Change history (system audit log: create/update/delete, who + when) for an object, newest first. `kind`/`ref` as `nbox_get`; `/api/core/object-changes/` (NetBox 4.x). Distinct from `nbox_journal` (operator notes): this is the system-recorded audit trail. Each row includes the top-level fields that changed (pre vs post); pass `diff=true` (pair with a small `limit`, e.g. 1) to include the full before/after JSON payloads per row. |
| `nbox_list_tags` | List tags (name, slug, color, usage count) — the valid `tag` values for `nbox_search`. |
| `nbox_tagged` | Objects carrying a tag, across all kinds (NetBox 4.3+); `tag` (id\|name\|slug). A cross-kind reverse lookup (\"what has tag X\") — unlike `nbox_search` with the `tag` filter, which narrows a free-text search per-endpoint. Each result carries `kind`/`object_type`/`id`/`display`/`url` + the resolved tag. |
| `nbox_cache_clear` | Drop nbox's local read cache so the next lookups fetch fresh from NetBox. Read-only with respect to NetBox (idempotent) — it only clears copies held in this server process; use after data changed out-of-band and you need current state before the TTL expires. |

`nbox_get` and `nbox_journal` take a `kind` and a `ref`. `kind` is one of
`device`, `ip`, `prefix`, `vlan`, `site`, `rack`, `rack_group`, `circuit`, `virtual_circuit`, `aggregate`,
`asn`, `ip_range`, `tenant`, `contact`, `provider`, `vm`, `vm_type`, `cluster`, `vrf`, `route_target`, `mac`, `interface` — both
tools accept the full set. `ref` is the natural reference for that kind: a
name/slug/ID for named objects, a CIDR for prefix and aggregate, an address for
ip, a VID or name for vlan, the AS number for asn, a name/RD/ID for vrf, a name (e.g. 65000:100) or ID for route_target, a MAC address (any common form, normalized) for mac, a `<device>/<name>` compound ref for interface (the name is taken verbatim after the device, since interface names may contain slashes), a CID or numeric ID for virtual_circuit.

`nbox_get` also accepts `ip_address` as an alias for `ip` — that's the `kind` a
`nbox_search` result carries, so a search → get chain can pass the hit's `kind`
through unchanged (it's the one kind whose spelling differs between the two).

## Resources

The same objects are also exposed as MCP **resources**, for hosts that browse or
attach resources rather than call tools. There is one resource template:

```
nbox://{kind}/{ref}
```

`kind` is the same set as `nbox_get` (`device`, `ip`, `prefix`, `vlan`, `site`,
`rack`, `rack_group`, `circuit`, `virtual_circuit`, `aggregate`, `asn`, `ip_range`, `tenant`, `contact`,
`provider`, `vm`, `vm_type`, `cluster`, `vrf`, `route_target`, `mac`, `interface`); `ref` is the same natural reference. Reading a resource returns the
object as JSON — the exact view model `nbox_get` returns. Examples:
`nbox://device/edge01`, `nbox://ip/10.0.0.1`, `nbox://site/iad1`,
`nbox://interface/edge01/xe-0/0/1`.

Percent-encode a `ref` that contains `/` — a CIDR is `nbox://prefix/10.0.0.0%2F24`.

It's a *template*, not a static list: `resources/list` is empty (enumerating
every NetBox object would mean walking the whole instance), and the template
carries the addressable shape. An unknown `kind`, a malformed URI, or a `ref`
that resolves to nothing returns an `invalid_params` error — the same
caller-fixable mapping `nbox_get` uses (an ambiguous `ref` can't be
disambiguated through a flat URI, so use `nbox_get` with `vrf`/`site`/`group` for
those).

## Prompts

`nbox serve` advertises a small catalog of **read-only investigation prompts**
(ROADMAP "MCP prompts catalog"). A prompt is a parameterized *plan* — a
structured set of steps naming the exact nbox tools to call — for a common
operator question. Discover them with `prompts/list`, expand one with
`prompts/get`:

| Prompt | Args | Investigates |
| ------ | ---- | ------------- |
| `ip_utilization_audit` | `site?`, `status?` | Prefixes ≥85% (near-full) and <10% (stale), with per-prefix recommendations. |
| `cable_path_trace` | `device`, `interface` | An interface's A-side ↔ Z-side cable path, through patch panels. |
| `find_stale_prefixes` | `site?` | Reclaimable prefixes (no/few assigned IPs), cross-checked against recent change history. |
| `object_change_review` | `kind`, `ref` | An object's recent audit-log changes, grouped by request, with risk flags (deletes, ownership/scope moves). |

`prompts/get` returns a single user-role message with the plan, tailored to the
supplied arguments (e.g. `cable_path_trace` with `device=edge01,
interface=xe-0/0/1` returns a plan that calls `nbox_get_interface` with
`device=edge01, interface=xe-0/0/1` and reads its `trace` field). No NetBox
round-trip — a prompt is a plan, not data; the agent runs the plan against the
tools. An unknown prompt name returns `invalid_params` listing the available
prompts.

## Security and behavior

- **Use a read-only NetBox token.** The server exposes no write path, but a
  read-only token is the real safety boundary — scope the token to what you want
  an agent to see.
- **stdout carries only the JSON-RPC stream.** All logging goes to stderr, so it
  never corrupts the protocol.
- **The token is never logged.** Request logging shows only the auth scheme
  marker (see [docs/CONFIG.md](CONFIG.md)).

## Troubleshooting

- **"no profile selected"** — set an active profile (`nbox profile use <name>`),
  or pass `--profile <name>` in the host's `args`.
- **Nothing happens / it seems to hang** — that's expected when run by hand. The
  server talks JSON-RPC over stdin/stdout and must be launched by an MCP host,
  not run in a terminal. Don't pipe anything else to its stdout.
- **Host can't find the binary** — give `command` an absolute path to `nbox`.
- **Connection errors** — run `nbox status` from a shell with the same env to
  isolate it from the host setup.
