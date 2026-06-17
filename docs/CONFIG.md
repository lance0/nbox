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
# refresh_secs = 30        # TUI auto-refresh (omit/0 = off)

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

1. `NBOX_TOKEN` (direct override)
2. the env var named by the profile's `token_env`

`auth_scheme = "auto"` detects NetBox 4.5+ v2 tokens (`nbt_…` → `Authorization:
Bearer`) versus legacy v1 tokens (`Authorization: Token`). Force one with
`bearer` or `token`. The token is never logged — request logging shows only the
scheme marker.

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
