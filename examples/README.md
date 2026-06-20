# Examples

Worked examples and ready-to-copy snippets for nbox.

- **[`config.toml`](config.toml)** — an annotated configuration: profiles,
  per-surface backends, the read cache, and the `[serve]` MCP block. Copy it to
  `~/.config/nbox/config.toml` and edit, or run `nbox config init` for a starter.

## MCP host config

Register the read-only MCP server with an MCP host. Claude Code:

```bash
claude mcp add nbox -e NBOX_TOKEN=nbt_xxx.yyy -- nbox serve
```

A generic host (e.g. Claude Desktop's JSON config) — the server is a subprocess
that speaks JSON-RPC over stdio:

```json
{
  "mcpServers": {
    "nbox": {
      "command": "nbox",
      "args": ["serve"],
      "env": { "NBOX_TOKEN": "nbt_xxx.yyy" }
    }
  }
}
```

## More

- Scripting, jq recipes, CI, exit codes: [docs/SCRIPTING.md](../docs/SCRIPTING.md)
- The MCP server in depth (HTTP, OIDC): [docs/MCP.md](../docs/MCP.md)
- How nbox compares to the web UI / raw API: [docs/COMPARISON.md](../docs/COMPARISON.md)
