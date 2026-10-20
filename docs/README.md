# nbox documentation

Find the page for the task at hand. The [project README](../README.md) is a
short overview; everything else lives here.

## Use nbox

| Page | Scope |
|------|-------|
| [Installation](INSTALL.md) | Install channels, build features, shell completions, man pages, first run, platform support. |
| [Features](FEATURES.md) | Every lookup and export, search filters, output, the TUI (keys, palette, Config modal, themes), CLI reference, global flags. |
| [Configuration](CONFIG.md) | `config.toml`, profiles, token resolution, GraphQL backends, UI settings, cache, `[serve]`, logging. |
| [Scripting](SCRIPTING.md) | Output formats, exit codes, `jq` recipes, CI, Docker. |
| [MCP server](MCP.md) | `nbox serve` for AI agents: host setup, tools, resources, prompts, write modes, HTTP, OIDC, token pass-through, Docker. |
| [Troubleshooting](TROUBLESHOOTING.md) | Symptoms and fixes for auth, config, TLS, output, and the MCP server. |
| [Comparison](COMPARISON.md) | nbox next to the NetBox web UI, `curl`, and `pynetbox`. |

## Reference

| Page | Scope |
|------|-------|
| [CLI reference](FEATURES.md#cli-reference) | Every command and its flags; [global flags](FEATURES.md#global-flags). |
| [Keybindings](FEATURES.md#keybindings) | TUI keys, [command palette](FEATURES.md#command-palette), [themes](FEATURES.md#themes). |
| [AGENTS.md](../AGENTS.md) | The machine-readable surface in one page: output flags, exit codes, commands, MCP tools. |
| [Compatibility](COMPATIBILITY.md) | NetBox 4.2 through 4.6: what changed in the API and how nbox adapts. |
| [Benchmarks](BENCHMARKS.md) | MCP cold-start against a Python NetBox MCP server. |
| [Known issues](../KNOWN_ISSUES.md) | Limitations and edge cases. |

## Project

| Page | Scope |
|------|-------|
| [Architecture](ARCHITECTURE.md) | Layers, data flow, output contracts, locked decisions. |
| [ADRs](adr/README.md) | Architecture decision records: context, decision, consequences. |
| [Continuous integration](CI.md) | Required PR checks, scheduled gates, local pre-tag smoke. |
| [Contributing](../CONTRIBUTING.md) | Development setup, checks, pull requests. |
| [Roadmap](../ROADMAP.md) | Shipped and planned work. |
| [Changelog](../CHANGELOG.md) | Release history. |
| [Security policy](../SECURITY.md) | Supported versions and how to report a vulnerability. |
| [Demo recording](demo.tape) | VHS source for the [TUI demo](demo.gif); re-render with `vhs docs/demo.tape`. |
