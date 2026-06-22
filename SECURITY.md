# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in nbox, please report it responsibly:

1. **Do not** open a public GitHub issue
2. Email lance@lance0.com or use GitHub's private vulnerability reporting
3. Include steps to reproduce the issue
4. Allow reasonable time for a fix before public disclosure

## Scope

Security issues of interest include:

- Leakage of the NetBox API token (in logs, error messages, or output)
- TLS verification bypass or weakening
- Memory-safety issues in API response parsing
- Any path that performs a write against NetBox (nbox is read-only)

## Security Posture

- **Token storage.** nbox resolves the API token in order: the env var named by the profile's `token_env`, then `NBOX_TOKEN`, then the profile's `token` value in `config.toml`. There is no OS keyring. If you prefer to keep the token out of the config file, use `token_env`/`NBOX_TOKEN` and store only the env-var *name* in the file. When a token is saved to `config.toml`, the file is written owner-only (`0600` on Unix) and the value is redacted from `config show` / `--json` / `Debug`. The token is never logged — request logging shows only the auth-scheme marker. Inspect the active source with `nbox config token status` (never prints the value).
- **Read-only.** Every command and MCP tool only reads; nbox issues no writes (`raw` is `GET`-only). Use a read-scoped NetBox token for defense in depth.
- **TLS verified by default.** `verify_tls = false` is supported for labs with self-signed certs but must not be used against production.
- **Clean stdout.** Data goes to stdout; logs and errors go to stderr — safe for piping and for the `nbox serve` JSON-RPC stream.

### MCP server (`nbox serve`)

`nbox serve` exposes the read-only tools to an MCP client. Its network surface:

- **stdio by default** — no network listener; the host launches nbox as a subprocess.
- **HTTP is loopback-only** unless OIDC is configured. `nbox serve --http <addr>` binds loopback and validates `Origin`/`Host` (a DNS-rebinding guard). An optional static bearer (`--http-token` / `NBOX_SERVE_TOKEN` — a secret; prefer the env var) gates `/mcp`.
- **A routable deployment is an OAuth 2.1 resource server.** A non-loopback bind requires `--oidc-issuer` + `--audience`: nbox validates inbound IdP JWTs on `/mcp` (signature via JWKS; `iss`/`aud`/`exp` checked; `alg: none` rejected) and advertises Protected Resource Metadata (RFC 9728). Terminate TLS in front (a reverse proxy).
- **Accountability, not per-user RBAC.** The last hop to NetBox still uses the single profile token, so scope that token read-only. An audit log (`nbox::audit`) records callers, and an optional per-caller rate limit (`--rate-limit`) bounds abuse.

See [docs/MCP.md](docs/MCP.md) for the full security model.

## Supported Versions

Only the latest release receives security updates. Upgrade to the most recent
version before reporting a vulnerability.
