# Troubleshooting

Symptom → cause → fix. The short table in the [README](../README.md#troubleshooting)
covers the common cases; this is the long form, grouped by where things go wrong.
Exit codes are stable: `0` success, `1` generic, `2` usage, `3` auth/permission,
`4` not found, `5` ambiguous reference. When in doubt, run `nbox status` first —
it confirms reachability, auth, and the NetBox version, and most failures repeat
there with a clearer message.

## Connection & auth

### `no config at … — run nbox config init`

First run, no config file yet. Launch the TUI with no arguments for the guided
wizard (it captures a profile, test-connects, and writes the file), or add a
profile from the shell:

```bash
nbox                                              # guided first-run wizard, then the TUI
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
```

### `authentication failed (HTTP 401)` (exit 3)

The token is missing, wrong, or expired — NetBox rejected it. First check which
source nbox resolved (it prints the source, never the token):

```bash
nbox config token status                          # token_env | NBOX_TOKEN | keyring | none
```

If it says `none`, no token was found. Set one and retry:

```bash
export NBOX_TOKEN=...                             # or point token_env at a set variable
nbox config token set                             # or store it in the OS keyring (input hidden)
```

If a token *is* resolving but still 401s, it's the wrong token for this instance —
regenerate it in NetBox.

### `permission denied (HTTP 403)` (exit 3)

The token is valid and authenticated, but lacks object permissions — 401 means
bad/missing token, 403 means authenticated-but-forbidden. Use a token whose
NetBox permissions include read access to the objects you're querying. A
read-scoped token with the right object permissions fixes it.

### Token resolution order

nbox resolves the token in a fixed order; the first hit wins:

1. the env var named by the profile's `token_env` (if set & present)
2. `NBOX_TOKEN`
3. the OS keyring entry for the profile (`nbox config token set`)
4. none — a clear "no token" error

Env always overrides the keyring. If a stale `NBOX_TOKEN` is exported it shadows a
correct keyring entry — `unset NBOX_TOKEN` (or fix the value), then re-check with
`nbox config token status`.

### Wrong Authorization scheme (v1 vs v2 token)

NetBox 4.5+ issues v2 tokens (`nbt_…`, sent as `Authorization: Bearer`); older
instances use v1 (`Authorization: Token`). `auth_scheme = "auto"` (the default)
detects which from the token shape. If detection guesses wrong for your setup,
pin it on the profile:

```toml
[profiles.work]
auth_scheme = "bearer"            # auto | bearer | token — force one
```

### `operation timed out` on one search endpoint

Transient. `nbox search` fans out ~17 requests at once; NetBox's sync gunicorn
workers close keep-alive connections, so nbox already disables stale pool reuse
(`pool_max_idle_per_host(0)`). If one endpoint still times out, retry, or raise
the per-request budget:

```toml
[profiles.work]
timeout_secs = 30                 # default 15
```

## Config & profiles

### `the backend key was removed`

The coarse `backend = "rest"|"graphql"` profile key is gone. Set the backend
per surface instead, under `[profiles.<name>.api]`. Only the VRF view is
GraphQL-capable; search is always REST (NetBox's GraphQL has no full-text `q`
equivalent), so a `search = "graphql"` preference transparently falls back:

```toml
[profiles.work.api]
vrf = "graphql"                   # rest | graphql — only vrf is GraphQL-capable
```

### Wrong instance / no active profile

You queried the wrong NetBox, or no profile is active. List them, switch the
active one, or override per command:

```bash
nbox profile list                 # active profile is marked
nbox profile use work             # persist the active profile
nbox device edge01 -p lab         # use a specific profile for one command
```

### Where is the config file

```bash
nbox config path                  # prints the resolved path
nbox config show                  # the effective config (secrets redacted)
```

It lives at `~/.config/nbox/config.toml` (Linux/macOS) or
`%APPDATA%\nbox\config.toml` (Windows). See [docs/CONFIG.md](CONFIG.md) for the
full schema.

## TLS

### Certificate error against a lab instance

A self-signed or otherwise untrusted certificate fails verification. For a lab
you control, disable verification on that profile only — never in production,
where it removes the protection TLS provides:

```toml
[profiles.lab]
verify_tls = false                # lab only — never in production
```

## Output & scripting

### `CSV output is only supported for tabular results (arrays)`

`-o csv` is for lists (e.g. `search`). A single object (e.g. `nbox device
edge01`) has no table shape, so it's rejected. Use JSON or plain text for one
object:

```bash
nbox device edge01 --json         # structured single object
nbox search edge -o csv           # CSV for the list result
```

### TUI hangs or won't launch in a script

A bare `nbox` (or `nbox tui`) launches the interactive UI and waits on a TTY,
which blocks in a non-interactive context. Pass `--no-tui` so it refuses with a
usage error (exit 2) instead of blocking, and call an explicit subcommand:

```bash
nbox --no-tui device edge01       # never drops into the TUI
```

### `… is ambiguous — matches: …` (exit 5)

The reference matched several objects. Disambiguate with the right scope flag —
`--vrf` for an IP or prefix that exists in several VRFs, `--site` or `--group`
for a VLAN VID present at several sites — or pass an exact ID:

```bash
nbox ip 10.0.0.1 --vrf mgmt       # the IP in the mgmt VRF
nbox vlan 100 --site iad1         # VID 100 at site iad1
```

## MCP server

See [docs/MCP.md](MCP.md#troubleshooting) for the server's own troubleshooting
notes; these expand on the transport and auth cases.

### Host can't launch the stdio server

The MCP host runs `nbox serve` as a subprocess and speaks JSON-RPC over its
stdin/stdout. It fails if the binary isn't found or no token is in the
subprocess's environment. Make sure `nbox` is on the host's `PATH` (or give the
host an absolute path), and that `NBOX_TOKEN` — or a usable profile — is set in
the env the host passes through. Logs go to stderr and JSON-RPC to stdout; never
mix anything else into stdout. Isolate setup problems by running the same query
from a shell with the same env:

```bash
nbox status                       # if this connects, the server will too
```

### HTTP `/mcp` returns 401

In loopback mode with a static bearer set, the request is missing or carrying the
wrong `Authorization: Bearer` token (`--http-token` / `NBOX_SERVE_TOKEN`). In
OIDC mode, the IdP JWT is invalid, expired, or its `aud` doesn't match
`--audience`. The most common OIDC cause: the IdP minted a token whose audience
isn't nbox — the IdP must mint the `aud` that equals `--audience` (via the RFC
8707 `resource` parameter), or every call 401s with a valid-looking token.

### Non-loopback bind refused (usage error)

`nbox serve --http` binds loopback only. A routable address (e.g. `0.0.0.0:8080`)
is a usage error unless OIDC resource-server mode is configured — there's no
bypass flag. Either bind loopback, or enable OIDC and terminate TLS in front:

```bash
nbox serve --http 127.0.0.1:8080                                   # loopback (default)
nbox serve --http 0.0.0.0:8080 \
  --oidc-issuer https://idp.example.com \
  --audience https://nbox.example.com                              # routable requires OIDC
```

### `429 Too Many Requests` / Retry-After

The per-caller `--rate-limit` (requests per minute) was exceeded. Back off until
the `Retry-After` window passes, or raise the cap:

```bash
nbox serve --http 127.0.0.1:8080 --rate-limit 240                  # raise the per-caller cap
```

### Origin or Host rejected (403)

The DNS-rebinding guard validates the `Host` (and `Origin`, when sent) header
against an allowed-host set. In loopback mode that set is loopback-only; in
OIDC/routable mode it's the `--audience` host plus loopback. Add another accepted
host (OIDC/routable mode only — it's ignored in loopback mode):

```bash
nbox serve --http 0.0.0.0:8080 \
  --oidc-issuer https://idp.example.com --audience https://nbox.example.com \
  --allowed-host nbox.internal.example.com
```

### `keyring not available on this system` (Linux)

The default static binary ships no D-Bus (Secret Service) backend, so
`nbox config token` can't reach a keyring. Use an env var instead, or install a
build with the Linux backend compiled in:

```bash
export NBOX_TOKEN=...                              # or set the profile's token_env
cargo install nbox --features keyring-secret-service   # build with the Linux keyring backend
```

## See also

- [docs/CONFIG.md](CONFIG.md) — config schema, profiles, token resolution, cache, logging
- [docs/MCP.md](MCP.md) — MCP server setup, tools, HTTP transport, and OIDC
