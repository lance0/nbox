# nbox

> Terminal UI and CLI for NetBox — fast search, IPAM lookups, device context, and (later) safe operational workflows.

**nbox** gives you k9s/lazygit-style navigation for NetBox data. It is built for the questions you actually ask at the terminal: *What is this IP? Where is this device? What prefix owns this address? What VLAN is this?* — and answers them fast, both interactively and as scriptable one-liners.

> ⚠️ **Status: pre-release / in active development.** v0.1 is read-only. See [ROADMAP.md](ROADMAP.md) for what's shipping when, and [DESIGN.md](DESIGN.md) for the full architecture.

---

## Quick Start

```bash
# Configure a profile
nbox config init
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
export NETBOX_TOKEN=...      # or NBOX_TOKEN to override

# Look things up from the shell
nbox search edge01
nbox device edge01
nbox ip 10.44.208.55
nbox prefix 10.44.208.0/24

# Or launch the interactive TUI
nbox
```

---

## Features

- **Fast shell lookups** — `device`, `ip`, `prefix`, `vlan`, `site`, `rack`, `interface`, `search`.
- **Normalized search** across devices, IPs, prefixes, VLANs, and sites in one command.
- **Interactive TUI** with search, detail panes, navigation history, and a command palette.
- **IPAM-aware** — IP → parent prefix → VLAN → site resolution, computed locally with `ipnet`.
- **Scriptable** — clean `--json` output on every command for piping into `jq`.
- **Open in browser** and **copy to clipboard** straight from results.
- **Profiles** for multiple NetBox instances (work, lab, …).
- **Read-only first**; safe `PATCH`-based writes with diff confirmation come later.

See [docs/FEATURES.md](docs/FEATURES.md) for the full list.

---

## Installation

> Published binaries and a Homebrew tap are planned (see [ROADMAP.md](ROADMAP.md)). For now, build from source.

```bash
# From source (requires Rust 1.88+)
git clone git@github.com:lance0/nbox.git
cd nbox
cargo install --path .
```

Optional features:

```bash
cargo install --path . --features cache,updates
```

| Feature     | Default | Description                          |
| ----------- | ------- | ------------------------------------ |
| `clipboard` | ✅      | Copy values with `y` (via `arboard`) |
| `cache`     | —       | Local SQLite cache (`rusqlite`)      |
| `updates`   | —       | GitHub update notifications          |

---

## Configuration

Config lives at:

| OS            | Path                        |
| ------------- | --------------------------- |
| Linux / macOS | `~/.config/nbox/config.toml` |
| Windows       | `%APPDATA%\nbox\config.toml` |

```toml
active_profile = "work"

[ui]
theme = "default"
confirm_writes = true

[profiles.work]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
auth_scheme = "auto"      # auto | bearer | token
verify_tls = true
timeout_secs = 15
page_size = 100
exclude_config_context = true
```

**Tokens are never stored in plaintext by default.** nbox reads them, in order, from:

1. `NBOX_TOKEN` (direct override)
2. the env var named by `token_env`
3. *(future)* OS keyring

nbox auto-detects v2 tokens (`Bearer nbt_<key>.<token>`) vs legacy v1 tokens (`Token <token>`). See [docs/CONFIG.md](docs/CONFIG.md).

---

## Usage

```bash
nbox                              # launch TUI
nbox search <query> [--limit N]
nbox device <name-or-id>
nbox ip <address>
nbox prefix <cidr>
nbox site <name-or-slug>
nbox rack <name-or-id>
nbox vlan <vid-or-name>
nbox interface <device> <interface>
nbox open <object-ref>
nbox completions <bash|zsh|fish|powershell|elvish>
```

Every command supports `--json`:

```bash
nbox device edge01 --json | jq '.primary_ip4.address'
nbox ip 10.44.208.55 --json
nbox search edge01 --limit 20
```

---

## TUI Keybindings

| Key                | Action                          |
| ------------------ | ------------------------------- |
| `/`                | search                          |
| `:`                | command palette                 |
| `Enter`            | open selected object            |
| `b` / `Esc`        | back                            |
| `o`                | open in browser                 |
| `y`                | copy selected field             |
| `r`                | refresh                         |
| `Tab` / `Shift+Tab`| next / previous pane            |
| `j`/`k`, `g`/`G`   | move, top / bottom              |
| `t`                | cycle theme                     |
| `?` / `F1`         | help                            |
| `q` / `Ctrl+c`     | quit                            |

On a device screen: `i` interfaces · `p` IPs · `c` cables · `v` VLANs.

---

## Security

- Tokens are sourced from the environment, not written to config by default.
- `verify_tls = false` is supported for labs but should not be used against production.
- Logs go to stderr or a file — never mixed into stdout — so JSON output stays pipe-safe.

See [SECURITY.md](SECURITY.md) for reporting vulnerabilities.

---

## NetBox Compatibility

- **Requires NetBox 4.2+** (uses the modern polymorphic `scope` model for prefixes/VLANs). nbox checks the instance version via `/api/status/` on connect.
- Targets the NetBox **REST API** (`/api/`) as the primary integration path.
- Supports both **v2 API tokens** (NetBox 4.5+, `Bearer`) and legacy **v1 tokens** (`Token`).
- Optional, read-only **GraphQL** (`/graphql/`) is used for nested detail views (v0.2+).

---

## Roadmap

v0.1 is read-only lookups + TUI. Writes (safe, `PATCH`-based, diff-confirmed) arrive in v0.2/v0.3. Full plan in [ROADMAP.md](ROADMAP.md).

---

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
