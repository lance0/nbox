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

- **Tokens are never written to config.** nbox reads the API token from `NBOX_TOKEN`, or the env var named by the profile's `token_env` — it is never persisted to disk and never logged (request logging shows only the auth scheme marker).
- **Read-only.** Every command and MCP tool only reads; nbox issues no writes. Use a read-only NetBox token for defense in depth.
- **TLS verified by default.** `verify_tls = false` is supported for labs but should not be used against production.
- **Clean stdout.** Data goes to stdout; logs and errors go to stderr — safe for piping and for the `nbox serve` JSON-RPC stream.

## Supported Versions

Only the latest release is supported with security updates.

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |
| < 0.1   | No        |
