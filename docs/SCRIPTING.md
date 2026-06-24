# Scripting & automation

nbox is built to be scripted. stdout carries only data — JSON, CSV, or plain
rows — and every log, diagnostic, and error goes to stderr, so a pipe never sees
noise. Exit codes are stable (an agent can branch on `$?`), and JSON is the
contract: the same view models the CLI prints are what the MCP server returns.

In any non-interactive context pass `--no-tui`. A bare `nbox` (or `nbox tui`)
launches the interactive TUI, which blocks on a terminal; `--no-tui` turns that
into a usage error (exit 2) instead — so a misfired invocation in CI fails fast
rather than hanging on a TTY that isn't there.

For scripts and agents, prefer resolving the token from the environment: export
`NBOX_TOKEN` (or point a profile's `token_env` at a set variable). Desktop users
may also store `token = "..."` in the profile. The full order is `token_env` →
`NBOX_TOKEN` → config `token` → none; env always wins. Requires NetBox 4.2+.

## Output formats

Every data command takes `-o plain|json|csv` (`--json` is shorthand for
`-o json`). The JSON family adds `--fields`, `--raw`, and `--envelope`.

| Flag | Effect |
|------|--------|
| (default) | `plain` — human/grep-friendly rows |
| `--json`, `-o json` | pretty JSON to stdout |
| `--raw` | compact JSON on one line (pairs with `--json` — small payloads, easy to log) |
| `--envelope` | wrap as `{ "schema_version": 1, "data": <payload> }` (forward-compatible parsing) |
| `--fields a,b,c` | keep only those top-level fields (applied per element for arrays) |
| `-o csv --cols a,b,c` | tabular/list results only — a single object is rejected (use `--json`) |

```bash
nbox device edge01                                   # plain: aligned key/value rows
nbox device edge01 --json                            # pretty JSON
nbox device edge01 --json --raw                      # one compact line
nbox device edge01 --json --envelope                 # { schema_version, data }
nbox device edge01 --json --fields name,status,site  # trim to three fields
nbox search edge -o csv --cols kind,display,url      # CSV table of hits
```

`-o csv` is for tabular/list results (e.g. `search`) — arrays render as a table.
A single object has no rows to tabulate, so nbox rejects it with a usage error
(exit 2); use `--json` or plain for one object.

Custom fields ride along on every detail lookup. In plain output each non-empty
custom field is a `cf.<name>` row; in JSON they collect under a `custom_fields`
object. Tags surface the same way — joined slugs in plain, a `tags` array in
JSON — and both are dropped when the object has none.

```bash
nbox device edge01 | grep '^cf\.'                    # plain: just the custom fields
nbox device edge01 --json | jq '.custom_fields'      # JSON: the custom_fields object
```

## Exit codes

Stable across releases, so a script can branch on them.

| Code | Meaning |
|------|---------|
| 0 | success |
| 1 | generic error (including other API failures) |
| 2 | usage error (bad arguments) |
| 3 | authentication / permission (HTTP 401/403) |
| 4 | not found (no object matched) |
| 5 | ambiguous reference (more than one match) |
| 141 | broken pipe (SIGPIPE — a downstream reader like `head` closed early) |

```bash
#!/usr/bin/env bash
# Treat "not found" as a soft miss; everything else is a hard error.
nbox device "$1" --no-tui --json > /tmp/dev.json
case $? in
  0) echo "found $1" ;;                              # ok
  4) echo "no such device: $1"; exit 0 ;;            # soft miss
  3) echo "auth failed — check NBOX_TOKEN" >&2; exit 3 ;;
  5) echo "ambiguous — scope with --vrf/--site/--group" >&2; exit 5 ;;
  *) echo "nbox error" >&2; exit 1 ;;                # generic
esac
```

## jq recipes

stdout is clean JSON, so jq does the rest. Field paths shown here that aren't
otherwise documented are illustrative — confirm them against your own
`nbox <cmd> --json` output before relying on them in a pipeline.

```bash
nbox device edge01 --json | jq -r '.primary_ip4'                 # device primary IPv4 (a string like 10.44.208.55/32)
nbox search edge -o csv --cols kind,display,url                  # CSV of search hits, three columns
nbox next-ip 10.44.208.0/24 --count 4 --json | jq -r '.available[]'    # next 4 free addresses, one per line
nbox next-prefix 10.0.0.0/8 --length 26 --json | jq -r '.available[0]' # first free /26
nbox tags --json | jq -r '.tags[] | "\(.name)\t\(.slug)"'             # tags as name<TAB>slug
nbox device edge01 --json --envelope | jq -r '.data.status'      # envelope-aware: read through .data
nbox device edge01 --json | jq -r '.custom_fields.owner // "-"'  # a custom field, default when absent (illustrative)
nbox search edge --partial --json | jq 'length'                  # count of hits (best-effort search)
```

When you parse with `--envelope`, always read through `.data` — the envelope is
`{ schema_version, data }`, and `schema_version` is there so a future shape
change doesn't silently break your parser.

## In CI

Use `--no-tui`, take the token from a secret in the environment, and prefer
`--envelope` so the parse target is stable across versions. A read-only check —
resolve an object, assert exit 0 — is enough to gate a deploy on "NetBox knows
about this thing."

GitHub Actions:

```yaml
name: NetBox sanity check

on:
  push:
    branches: [main]

jobs:
  netbox-check:
    runs-on: ubuntu-latest
    steps:
      - name: Install nbox
        run: cargo install nbox                       # or download a release binary

      - name: Resolve a known prefix (read-only)
        env:
          NBOX_TOKEN: ${{ secrets.NBOX_TOKEN }}       # token from a secret, never a file
          NBOX_URL: ${{ vars.NBOX_URL }}
        run: |
          nbox profile add ci "$NBOX_URL" --token-env NBOX_TOKEN
          nbox profile use ci
          nbox prefix 10.44.208.0/24 --no-tui --json --envelope > prefix.json
          jq -e '.data.prefix' prefix.json            # exit non-zero if the field is missing
```

Portable shell (any CI):

```bash
set -euo pipefail
export NBOX_TOKEN="$CI_NBOX_TOKEN"                     # from the CI secret store
nbox prefix 10.44.208.0/24 --no-tui --json --envelope \
  | jq -e '.data.prefix' > /dev/null                  # assert it resolved; fail the job otherwise
```

## Docker

The image is on GHCR (multi-arch, amd64/arm64). Pass the token as an env var and
the subcommand as the container arguments:

```bash
docker run --rm -e NBOX_TOKEN=... \
  ghcr.io/lance0/nbox:latest device edge01 --no-tui --json   # one-shot lookup
```

The image needs a profile (a URL) too. Either mount a config and point at it, or
pass `--config`:

```bash
docker run --rm -e NBOX_TOKEN=... \
  -v "$HOME/.config/nbox:/root/.config/nbox:ro" \
  ghcr.io/lance0/nbox:latest --config /root/.config/nbox/config.toml \
  ip 10.44.208.55 --no-tui --json                            # config mounted read-only
```

## For AI agents (MCP)

`nbox serve` is a read-only MCP server. An MCP host launches it as a subprocess
and speaks JSON-RPC over stdin/stdout (logs stay on stderr). The tools reuse the
CLI's query and view layer, so they return the exact same JSON view models the
CLI prints — an agent gets the structured data, not screen-scraped text.

Local stdio host (Claude Code):

```bash
claude mcp add nbox -e NBOX_TOKEN=nbt_xxx.yyy -- nbox serve   # register nbox as a subprocess
```

Generic MCP host config (e.g. Claude Desktop's JSON):

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

The ten tools, all annotated read-only:

| Tool | What |
|------|------|
| `nbox_status` | Connection + backend capabilities + NetBox/Django/Python versions (call first). |
| `nbox_search` | Search across object types; `query` (required) plus scope/filter args. |
| `nbox_get` | Fetch one object by `kind` + `ref` (`vrf`/`site`/`group` disambiguate). |
| `nbox_get_interface` | One interface on a device, with its cable-path trace. |
| `nbox_next_ip` | Next available address(es) in a prefix (nothing is reserved). |
| `nbox_next_prefix` | Available free child prefix(es) of a given length. |
| `nbox_journal` | Recent journal entries for an object. |
| `nbox_list_tags` | List tags (name, slug, color, usage count). |
| `nbox_tagged` | Objects carrying a tag, across kinds (NetBox 4.3+); `tag` (id\|name\|slug). |
| `nbox_cache_clear` | Drop nbox's local read cache so the next reads fetch fresh. |

The same objects are also exposed as MCP resources via one template,
`nbox://{kind}/{ref}` (e.g. `nbox://device/edge01`) — reading one returns the
same view `nbox_get` does, for hosts that browse resources instead of calling
tools.

HTTP transport, OIDC resource-server mode, the audit log, and rate limiting are
covered in [MCP.md](MCP.md).

## Tips

- Search fails closed by default: if any endpoint errors, `search` exits
  non-zero rather than return a partial result you might mistake for complete.
  Pass `--partial` to accept best-effort results (failed endpoints are reported
  on stderr) — useful for an agent that would rather have most of the answer.
- Prefer `--envelope` in anything long-lived. The `schema_version` field is the
  forward-compat handle; parse through `.data`.
- Use `--fields` to trim payloads for agents — fewer tokens, less to reason over.
- Always `--no-tui` in automation, so a stray bare invocation can never block on
  a terminal.
- Keep stdout for data only. Logs already go to stderr; if you need them on disk,
  use `--log-file <path>` (it tees to the file and stderr, never stdout). The
  level resolves `--log-level` → config `log_level` → `NBOX_LOG` → `RUST_LOG` →
  `warn`.
