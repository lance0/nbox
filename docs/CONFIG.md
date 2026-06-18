# Configuration

Config lives at `~/.config/nbox/config.toml` (Linux/macOS) or
`%APPDATA%\nbox\config.toml` (Windows). Create it with `nbox config init`;
inspect the resolved path with `nbox config path` and the effective config with
`nbox config show`. A full example is in [`examples/config.toml`](../examples/config.toml).

## Shape

```toml
config_version = 1
active_profile = "work"

# Logging (optional). Omit both to log to stderr at `warn` (the default).
# log_file  = "/var/log/nbox.log"   # also write logs here; stdout stays clean
# log_level = "info"                # warn | info | debug | trace | nbox=debug

[ui]
theme = "default"
confirm_writes = true
# refresh_secs = 30          # TUI auto-refresh interval in seconds (omit/0 = off)
# open_browser_command = ""  # custom browser-open command (empty = OS default)

[profiles.work]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
auth_scheme = "auto"          # auto | bearer | token
verify_tls = true
timeout_secs = 15
page_size = 100
exclude_config_context = true
```

`config_version` is written by `config init`. A config with a *newer* version
than your nbox build still loads, with a warning — older builds won't silently
mishandle a newer schema.

## Tokens

Tokens are **never written to config**. nbox resolves them in order:

1. the env var named by the profile's `token_env` (if set & present)
2. `NBOX_TOKEN`
3. the OS keyring entry for the profile (`nbox config token set`)
4. none — nbox reports a clear "no token" error

Env always overrides the keyring: CI/SSH/break-glass paths set an env var, while
the keyring is for interactive human onboarding. Inspect the active source with
`nbox config token status` (it prints the source — `token_env`/`NBOX_TOKEN`/
`keyring`/`none` — never the token).

### OS keyring

Store the token in your OS keyring instead of an env var:

```bash
nbox config token set      # prompts, input hidden (or reads a piped line)
nbox config token status   # shows the resolved source, never the token
nbox config token clear    # removes the stored token
```

`set`/`clear` act on the active profile (or `--profile <name>`). The token is read
without echo from a TTY prompt, or as a single line from stdin when piped
(scripting) — there is no positional token argument, so it can't leak into shell
history. The entry is keyed by config path + profile name (service `nbox`).

Backends: macOS Keychain and Windows Credential Manager are built in. On Linux
the Secret Service (D-Bus) backend is **off by default** — build with
`--features keyring-secret-service` to enable it; otherwise `nbox config token`
reports the keyring as unavailable and you should use `NBOX_TOKEN` or a
`token_env` instead. (This keeps static/musl builds free of a D-Bus link
dependency.)

`auth_scheme = "auto"` detects NetBox 4.5+ v2 tokens (`nbt_…` → `Authorization:
Bearer`) versus legacy v1 tokens (`Authorization: Token`). Force one with
`bearer` or `token`. The token is never logged — request logging shows only the
scheme marker.

## UI settings

The `[ui]` table holds TUI preferences. Three are editable in-app from the Config
modal's **Settings** section (`Tab` to it; `↑`/`↓` move between fields; `Enter` or
`Ctrl+S` saves) — saving writes them back to `config.toml` format-preserving:

- `theme` — the TUI color theme. Cycle it in the Settings section (`←`/`→`/Space,
  applied live), with the `t` key, or the palette `:theme <name>` verb. Disabled
  under `NO_COLOR`.
- `refresh_secs` — TUI auto-refresh interval in seconds; omit or `0` to disable.
  Changing it in the Settings section re-arms the refresh without a restart.
- `open_browser_command` — a custom command to open URLs (`nbox open` and the TUI
  `o` action). Split into program + args, with the URL appended as a literal final
  argument (never shell-interpolated); empty uses the OS default opener. The TUI
  reads the live value, so a change applies to the next `o`.

`confirm_writes` is reserved for the future write features and has no effect today,
so it is not exposed in the Settings section. (The former `wide` knob was removed —
nothing read it; an existing `wide = …` in your file is harmlessly ignored.)

## Profiles

Each `[profiles.<name>]` is a NetBox instance. Manage them with:

```bash
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
nbox profile list
nbox profile show [name]
```

Pick a profile per-invocation with `--profile <name>`, or point at an alternate
file with `--config <path>`.

## MCP server (`[serve]`)

The optional `[serve]` table holds defaults for `nbox serve` (the MCP server).
Absent ⇒ stdio (no HTTP). The matching CLI flags always override these keys.

| Key | Effect | Flag |
|-----|--------|------|
| `http` | Loopback address to serve HTTP on, e.g. `127.0.0.1:8080`. Absent ⇒ stdio. | `--http` |
| `http_token` | Static bearer token required on `/mcp`. **A secret** — prefer the env var over storing it here; `nbox config show` redacts it. | `--http-token` / `NBOX_SERVE_TOKEN` |
| `oidc_issuer` | OIDC issuer URL. Its presence switches HTTP into OAuth 2.1 resource-server mode (inbound IdP JWTs validated on `/mcp`). | `--oidc-issuer` |
| `audience` | Expected token audience (nbox's canonical resource URI). Required when `oidc_issuer` is set. | `--audience` |
| `jwks_url` | JWKS URL override; absent ⇒ discover from the issuer. | `--oidc-jwks-url` |
| `allowed_hosts` | Extra hostnames for the DNS-rebinding allow-list (OIDC/routable mode only), on top of the audience host + loopback. | `--allowed-host` |
| `rate_limit` | Per-caller request cap on `/mcp`, in requests per minute. Absent / `0` ⇒ disabled. | `--rate-limit` |

```toml
[serve]
http = "127.0.0.1:8080"
# http_token is a SECRET — prefer NBOX_SERVE_TOKEN over storing it in the file.
# http_token = "..."
# OIDC resource-server mode (routable deployments):
# oidc_issuer   = "https://idp.example.com"
# audience      = "https://nbox.example.com"
# jwks_url      = "https://idp.example.com/keys"
# allowed_hosts = ["nbox.example.com"]
# rate_limit    = 120
```

See [docs/MCP.md](MCP.md) for the full server story.

## Logging

Two top-level, optional keys control logging:

| Key | Effect |
|-----|--------|
| `log_file` | Path to a log file. When set, logs are written there **and** mirrored to stderr; absent, stderr only. |
| `log_level` | `tracing` filter — `warn` (default), `info`, `debug`, `trace`, or a per-target spec like `nbox=debug`. |

Precedence, highest first:

- **File**: `--log-file` flag → config `log_file` → none (stderr only).
- **Level**: `--log-level` flag → config `log_level` → `NBOX_LOG` → `RUST_LOG` → `warn`.

The file is opened directly at the path you give — a literal path (no `~`
expansion, no date-rolling suffix) — via a non-blocking background writer; the
parent directory is created if needed.
**stdout is never used for logs** — it's reserved for command output, so
`--json` and `nbox serve` stay pipe-safe.
