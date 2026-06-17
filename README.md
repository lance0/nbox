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

<!-- Demo: replace with an asciinema/VHS recording before the v0.1 release.
     e.g. [![asciicast](https://asciinema.org/a/<id>.svg)](https://asciinema.org/a/<id>)
     or a docs/demo.gif rendered with `vhs docs/demo.tape`. -->
> 📽️ _A short asciinema/VHS demo lands with the v0.1 release._

---

## Features

- **Fast shell lookups** — `device`, `ip`, `prefix`, `vlan`, `site`, `rack`, `interface`, `search`.
- **Normalized search** across devices, IPs, prefixes, VLANs, sites, circuits, aggregates, ASNs, and IP ranges in one command.
- **Interactive TUI** with search, detail panes, navigation history, and a command palette.
- **IPAM-aware** — IP → parent prefix → VLAN → scope (site/location/region/…) resolution, computed locally with `ipnet`.
- **Scriptable** — clean `--json` output on every command for piping into `jq`.
- **Open in browser** and **copy to clipboard** straight from results.
- **Profiles** for multiple NetBox instances (work, lab, …).
- **Scriptable / agent-friendly** — `-o json|csv|plain`, `--fields`, `--raw`, versioned `--envelope`, and stable exit codes (see [AGENTS.md](AGENTS.md)).
- **MCP server** — `nbox serve` exposes the lookups as read-only MCP tools over stdio.
- **Read-only first**; safe `PATCH`-based writes with diff confirmation come later.

See [docs/FEATURES.md](docs/FEATURES.md) for the full command reference and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the internals.

---

## Installation

```bash
# From crates.io (requires Rust 1.88+)
cargo install nbox

# Or grab a prebuilt binary (Linux/macOS) — downloads the latest release,
# falls back to `cargo install` if there's no asset for your platform
curl -fsSL https://raw.githubusercontent.com/lance0/nbox/master/scripts/install.sh | sh
```

> Prebuilt binaries for Linux (x86_64/aarch64), macOS (Intel/ARM), and Windows
> are attached to each [GitHub Release](https://github.com/lance0/nbox/releases).
> A Homebrew tap formula template lives in [`packaging/homebrew/`](packaging/homebrew/nbox.rb).

Build from source:

```bash
git clone git@github.com:lance0/nbox.git
cd nbox
cargo install --path . --features cache,updates   # optional features
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
config_version = 1
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

nbox auto-detects v2 tokens (`Bearer nbt_<key>.<token>`) vs legacy v1 tokens (`Token <token>`). See [docs/CONFIG.md](docs/CONFIG.md) for the full config reference.

---

## Usage

```bash
nbox                              # launch TUI
nbox status                       # connection + NetBox/Django/Python versions
nbox search <query> [--limit N] [--status/--site/--tenant/--role/--tag <v>] [--cols a,b,c] [--partial]
nbox tags                         # list tags (slug, name, count)
nbox device <name-or-id> [--journal] [--journal-limit N]
nbox ip <address> [--vrf <name>] [--journal]  # --vrf disambiguates duplicates across VRFs
nbox prefix <cidr> [--vrf <name>] [--journal] # includes utilization + children when present
nbox next-ip <cidr> [--count N] [--vrf <name>]      # next available address(es)
nbox next-prefix <cidr> [--length L] [--vrf <name>] # available free block(s)
nbox site <name-or-slug> [--journal]
nbox rack <name-or-id> [--journal]
nbox circuit <cid-or-id> [--journal]
nbox aggregate <cidr-or-id> [--journal]
nbox asn <number> [--journal]
nbox ip-range <start-or-id> [--journal]
nbox vlan <vid-or-name> [--site <s>] [--group <g>] [--journal]
nbox interface <device> <interface>
nbox journal <kind> <ref>         # recent journal entries for an object
                                  # --journal folds recent entries into a detail lookup (cap 5)
                                  # --journal-limit N overrides the cap (implies --journal)
nbox open <kind>/<ref>            # device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip-range
nbox raw GET <api-path>           # raw read-only API request (escape hatch)
nbox serve                        # read-only MCP server over stdio (for AI agents)
nbox config <init|path|show>
nbox profile <add|use|list|show>
nbox completions <bash|zsh|fish|powershell|elvish>
nbox man                          # generate a man page: nbox man > nbox.1
```

### Global flags

These apply to every command:

| Flag                       | Effect                                                          |
| -------------------------- | -------------------------------------------------------------- |
| `-o, --output <fmt>`       | `plain` (default), `json`, or `csv`                            |
| `--json`                   | Shortcut for `-o json`                                         |
| `--fields a,b,c`           | JSON: keep only these top-level fields                         |
| `--raw`                    | JSON: compact (single line) instead of pretty                 |
| `--envelope`               | JSON: wrap as `{ schema_version, data }`                       |
| `-p, --profile <name>`     | Use a specific profile for this invocation                     |
| `--config <path>`          | Use an alternate config file                                   |
| `--log-level <spec>`       | `tracing` filter to stderr (`info`, `debug`, `nbox=debug`, …) |
| `--no-tui`                 | Never fall through to the interactive TUI                      |

Custom fields appear as `cf.<name>` rows (plain) and a `custom_fields` object (JSON).

```bash
nbox device edge01 --json | jq '.primary_ip4.address'
nbox ip 10.44.208.55 --json
nbox search edge01 --limit 20 --status active
nbox search edge01 -o csv --cols name,site,status > devices.csv
nbox prefix 10.44.208.0/24 --envelope --raw      # versioned, single-line JSON
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

On a device screen: `i` interfaces · `p` IPs · `c` cables · `v` VLANs · `s` services.

The **command palette** (`:`) accepts `device`/`ip`/`prefix`/`vlan`/`site <ref>`, `find <q>` (or bare text), `open`, `copy`, `theme <name>`, and `refresh`. The **home screen** lists recently opened objects (deduped, most-recent-first) when there are no search results — press `Enter` to reopen one. Set `[ui].refresh_secs` to auto-refresh the current search on an interval (off by default), preserving your selection.

---

## MCP server

`nbox serve` runs a read-only [MCP](https://modelcontextprotocol.io) server over the stdio transport. An MCP host (Claude Desktop, Claude Code, …) launches `nbox serve` as a subprocess and speaks JSON-RPC over stdin/stdout; it reuses the same query + view layer as the CLI, so the tools return the same JSON view models. NetBox URL and token come from the active profile / env, and it takes the same global flags (`-p/--profile`, `--config`). JSON-RPC goes to stdout; all logging stays on stderr.

The tools are all annotated read-only:

| Tool | What |
| ---- | ---- |
| `nbox_status` | Connection + NetBox/Django/Python versions. |
| `nbox_search` | Search devices/IPs/prefixes/VLANs/sites/circuits/aggregates/ASNs/IP-ranges; `query` (required), `limit`, `status`, `site`, `tenant`, `role`, `tag`. |
| `nbox_get` | Fetch one object by `kind` (device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range) + `ref`; `vrf`/`site`/`group` disambiguate. |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix. |
| `nbox_next_prefix` | Next available child prefix(es) of a given length. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_list_tags` | List tags. |

Full setup: [docs/MCP.md](docs/MCP.md).

HTTP transport, OAuth, a raw escape-hatch tool, and MCP resources/prompts come later.

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
