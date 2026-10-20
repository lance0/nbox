# nbox

[![Crates.io](https://img.shields.io/crates/v/nbox.svg)](https://crates.io/crates/nbox)
[![CI](https://github.com/lance0/nbox/actions/workflows/ci.yml/badge.svg)](https://github.com/lance0/nbox/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-tip-ff5e5b?logo=ko-fi)](https://ko-fi.com/lance0)

**Terminal UI and CLI for NetBox: fast search, IPAM lookups, and device context.**

nbox answers the questions you ask at the terminal (what is this IP, where is
this device, what owns this prefix) from the shell, a k9s-style TUI, or an MCP
server for AI agents. It is one static binary against NetBox 4.2+. Reads are the
default; a small set of safe writes (reserve an IP or prefix, set a status or
description, add or remove a tag) runs plan-first behind `--allow-writes` and a
confirmation. nbox is pre-1.0.

[Documentation](docs/README.md) · [Install](#install) ·
[Releases](https://github.com/lance0/nbox/releases) · [Changelog](CHANGELOG.md)

<p align="center">
  <img src="docs/demo.gif" alt="nbox TUI walkthrough: ranked search, a device's interfaces and IP addresses, and the hierarchical prefix tree" width="900">
</p>

## Quick start

```bash
cargo install nbox                    # or: brew install lance0/tap/nbox
nbox                                  # first run: guided setup, then the TUI; press ? for keys
```

Or set up a profile from the shell and look things up directly:

```bash
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
export NETBOX_TOKEN=...

nbox search edge01                    # ranked search across every object type
nbox ip 10.44.208.55                  # address, parent prefix, VLAN, scope
nbox device edge01 --json             # device, interfaces, IPs, cables, VLANs
nbox next-prefix 10.0.0.0/8 --length 26
```

## What it does

<a id="features"></a><a id="real-world-use-cases"></a><a id="nbox-vs-the-netbox-web-ui-vs-raw-api"></a>
<a id="what-is-this-ip"></a><a id="where-is-this-device-and-whats-on-it"></a>
<a id="find-the-next-free-address-or-block"></a><a id="pull-data-into-a-script"></a>
<a id="usage"></a><a id="global-flags"></a><a id="keybindings-tui"></a><a id="themes"></a>

| Task | Command or guide |
|------|------------------|
| What is this IP? | `nbox ip 10.44.208.55`: the address, its most-specific parent prefix (VRF-scoped), that prefix's VLAN and scope. `--vrf` when the address is in several VRFs. |
| Where is this device, and what's on it? | `nbox device edge01`: interfaces, IPs, cables, VLANs, and services in one lookup. `nbox interface edge01 xe-0/0/1` traces the cable path A↔Z. |
| Find something by name | `nbox search edge`: one ranked, deduped query across devices, IPs, prefixes, VLANs, circuits, VMs, and more, with `--site`/`--tenant`/`--tag`/`--vrf` filters. |
| Find the next free address or block | `nbox next-ip 10.44.208.0/24 --count 4`, `nbox next-prefix 10.0.0.0/8 --length 26` (read-only). Claim one with `nbox ip reserve` / `nbox prefix reserve`. |
| Pull data into a script | `--json`, `--fields`, `--envelope`, `-o csv`, stable exit codes. [Scripting](docs/SCRIPTING.md) |
| Export for other tools | `nbox export prometheus-sd`, `address-list`, `device-inventory`. [Structured exports](docs/FEATURES.md#structured-exports) |
| Browse interactively | The TUI: navigation rail, live preview, prefix tree, dashboard, cross-object jumps, twelve themes. [Keybindings](docs/FEATURES.md#keybindings) |
| Change something safely | `--dry-run` previews a plan; `--allow-writes --confirm` applies it. [Features](docs/FEATURES.md#lookups) · [ADR-0001](docs/adr/0001-safe-write-foundation.md) |

Every command and flag is in the [CLI reference](docs/FEATURES.md#cli-reference).
For when to use nbox versus the NetBox web UI, `curl`, or `pynetbox`, see the
[comparison](docs/COMPARISON.md).

## For agents

<a id="mcp-server"></a><a id="drive-it-from-an-agent"></a><a id="add-it-to-claude"></a><a id="http-transport-and-oidc"></a>

`nbox serve` is an MCP server, read-only by default. It returns the same JSON
view models the CLI prints, through eleven read tools and an
`nbox://{kind}/{ref}` resource template. As a static binary it is MCP-ready in
about 9 ms ([benchmarks](docs/BENCHMARKS.md)).

```bash
claude mcp add nbox -e NBOX_TOKEN=nbt_xxx.yyy -- nbox serve
nbox serve --print-config             # mcpServers JSON for other hosts
```

Writes over MCP are opt-in: `--local-writes` for a local stdio host using your
profile token, or `--allow-writes` over HTTP/OIDC with a per-user token vault.
For a shared multi-user server,
[`--netbox-token-passthrough`](docs/MCP.md#netbox-api-token-pass-through-multi-user)
forwards each caller's own NetBox token, so NetBox permissions apply per user.
Host setup, tools, transports, and the security model are in
[docs/MCP.md](docs/MCP.md). Agents driving the CLI directly should read
[AGENTS.md](AGENTS.md); [SKILL.md](SKILL.md) is an installable Agent Skill.

## Install

<a id="installation"></a><a id="from-cratesio-recommended"></a><a id="homebrew-macoslinux"></a><a id="docker-ghcr"></a>
<a id="pre-built-binaries"></a><a id="quick-install-script"></a><a id="from-source"></a>
<a id="build-features"></a><a id="shell-completions"></a>

The [installation guide](docs/INSTALL.md) covers every channel, build features,
shell completions, and man pages.

- **Cargo:** `cargo install nbox` (Rust 1.88+)
- **Homebrew (macOS, Linux):** `brew install lance0/tap/nbox`
- **Install script (Linux, macOS):** `curl -fsSL https://raw.githubusercontent.com/lance0/nbox/master/scripts/install.sh | sh` ([review it first](scripts/install.sh))
- **Pre-built binaries:** Linux x86_64/ARM64, macOS, and Windows from [GitHub Releases](https://github.com/lance0/nbox/releases), with `SHA256SUMS`
- **Docker:** `docker run --rm ghcr.io/lance0/nbox:latest --help`

## Platform support

<a id="netbox-compatibility"></a>

Release binaries cover Linux (x86_64, ARM64), macOS (Apple Silicon, Intel), and
Windows x86_64; the GHCR image is amd64/arm64. nbox needs NetBox **4.2+** and is
verified through 4.6. It adapts to API differences between releases at runtime;
see [compatibility](docs/COMPATIBILITY.md).

## Documentation

<a id="configuration"></a><a id="troubleshooting"></a>

Start with the [documentation index](docs/README.md), or go directly to your task:

| Task | Guide |
|------|-------|
| Install and set up | [Installation](docs/INSTALL.md) · [Configuration](docs/CONFIG.md) · [Tokens](docs/CONFIG.md#tokens) |
| Use the TUI | [Keybindings](docs/FEATURES.md#keybindings) · [Config modal](docs/FEATURES.md#config-modal) · [Themes](docs/FEATURES.md#themes) |
| Script or automate | [Scripting](docs/SCRIPTING.md) · [CLI reference](docs/FEATURES.md#cli-reference) · [AGENTS.md](AGENTS.md) |
| Connect an AI agent | [MCP server](docs/MCP.md) |
| Troubleshoot | [Troubleshooting](docs/TROUBLESHOOTING.md) · [Known issues](KNOWN_ISSUES.md) |
| Compare with other tools | [Comparison](docs/COMPARISON.md) (NetBox web UI, `curl`, `pynetbox`) |
| Understand internals or contribute | [Architecture](docs/ARCHITECTURE.md) · [ADRs](docs/adr/README.md) · [CI](docs/CI.md) · [Contributing](CONTRIBUTING.md) · [Roadmap](ROADMAP.md) · [Security](SECURITY.md) |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at
your option.
