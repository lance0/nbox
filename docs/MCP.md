# MCP server

`nbox serve` runs a read-only [MCP](https://modelcontextprotocol.io) (Model
Context Protocol) server over the stdio transport. An MCP host launches the
`nbox` binary as a subprocess and speaks JSON-RPC over its stdin/stdout; the
tools reuse the same NetBox query + view layer as the CLI, so they return the
same JSON view models. Nothing is ever written.

## Prerequisites

A configured profile, exactly as the CLI needs one: a NetBox `url` and a token.
`nbox serve` resolves the token the same way every other command does — from
`NBOX_TOKEN`, or the env var named by the profile's `token_env` — and it honors
the same global flags (`-p`/`--profile <name>`, `--config <path>`). See
[docs/CONFIG.md](CONFIG.md) for profiles and token resolution. Confirm the CLI
works first:

```bash
nbox status
```

If that connects, the MCP server will too — it uses the same path.

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

## Tools

All tools are annotated read-only.

| Tool | Purpose |
| ---- | ------- |
| `nbox_status` | Connection target plus NetBox/Django/Python versions. Call first to confirm reachability. |
| `nbox_search` | Free-text search across devices, sites, IPs, prefixes, VLANs, circuits, aggregates, ASNs, and IP ranges. Optional `limit`, `status`, `site`, `tenant`, `role`, `tag` filters. Use it to find an object's exact reference. |
| `nbox_get` | Fetch one object by `kind` + `ref`. An ambiguous `ref` returns a candidate list; pass `vrf` (ip/prefix) or `site`/`group` (vlan) to disambiguate. |
| `nbox_get_interface` | One interface on a device: its config, assigned addresses, and cable-path trace. |
| `nbox_next_ip` | Next available address(es) within a prefix. `count`, `vrf`. Nothing is reserved. |
| `nbox_next_prefix` | Available child prefix(es) within a prefix. `length` returns the first free block of that size, else all free blocks. `vrf`. Nothing is reserved. |
| `nbox_journal` | Recent journal entries for an object, newest first. `kind`/`ref` as `nbox_get`. |
| `nbox_list_tags` | List tags (name, slug, color, usage count) — the valid `tag` values for `nbox_search`. |

`nbox_get` and `nbox_journal` take a `kind` and a `ref`. `kind` is one of
`device`, `ip`, `prefix`, `vlan`, `site`, `rack`, `circuit`, `aggregate`,
`asn`, `ip_range` (journal supports the first seven). `ref` is the natural
reference for that kind: a name/slug/ID for named objects, a CIDR for prefix and
aggregate, an address for ip, a VID or name for vlan, the AS number for asn.

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
