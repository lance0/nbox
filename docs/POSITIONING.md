# nbox — Positioning & Strategy

> Status: working strategy (2026-06-16). Distilled from competitive + community research.
> The companion plan lives in [ROADMAP.md](../ROADMAP.md). This doc is the "why/where we win."

## One-liner

> **k9s for NetBox** — answer *what is this IP? where is this device? what prefix/VLAN owns it?* in keystrokes, not browser clicks. A fast Rust TUI + scriptable CLI for NetBox 4.2+.

Lead with the **human TUI**. Agent-readiness is proof we're modern, not the headline (see "The agent angle").

## The wedge (the market gap)

The defining finding of the research: **no mature, interactive NetBox TUI exists in any language.**

- The closest direct competitor, `Hebbian-Robotics/nbx` (crates.io `nbx` 4.6.0), is **CLI-only and effectively abandoned** — ~1 star, ~30 downloads, no commits since launch (2026-05-08). It validated demand for an "agent-friendly NetBox CLI" and then stalled. It explicitly does not do the interactive half.
- The only other "TUI" (`emersonfelipesp/netbox-sdk`, Python) is pre-alpha (0.0.x) with no documented navigation/keybindings/fuzzy/palette/themes.
- The live Rust CLI (`cyberwitchery/netbox.rs`) is a general API wrapper — endpoint-shaped, no TUI, no lookup-first verbs.

And the pain is documented in NetBox's own tracker:

- Device-by-IP search is an **accepted ~30s performance bug** ([netbox#21196](https://github.com/netbox-community/netbox/issues/21196)).
- Global search **regressed to context-free**; users publicly abandoned it for manual table-filtering ([netbox#12314](https://github.com/netbox-community/netbox/discussions/12314)).
- IPAM browsing is "minutes per query" at scale ([netbox#21255](https://github.com/netbox-community/netbox/discussions/21255), [#21396](https://github.com/netbox-community/netbox/issues/21396)).

We are the first credible entrant in the *interactive* category, the incumbent tool is asleep, and the platform vendor (NetBox Labs) has left "free, self-hosted, human-first TUI with safe writes" wide open.

## Target users

- **Primary (acute pain): NOC / ops / datacenter engineers** doing dozens of "what is this IP / where is this device" lookups a day. Hit hardest by the slow UI + weak search; least served by today's scripting-oriented CLIs.
- **Beachhead adopter: network-automation engineers** — already terminal-native; will love `--json`, filter validation, single static binary.
- Secondary: SREs/DevOps with a network remit; smaller infra teams / WISPs.

## Competitive landscape (condensed)

| Tool | Lang | Shape | State | Why we beat it |
|---|---|---|---|---|
| Hebbian-Robotics/`nbx` | Rust | CLI (agent-pitched) | Abandoned (1★, dormant) | No TUI; we absorb its agent-friendly design *and* ship the TUI it refuses to build |
| `cyberwitchery/netbox.rs` (`netbox-cli`) | Rust | lib+CLI | Active, 4★ | Endpoint/CRUD-shaped, no TUI, no lookup-first verbs |
| `nbcli` | Python | CLI + REPL | Perpetual alpha, 44★ | Needs interpreter; "shell" is a REPL, not a TUI; no JSON-first |
| `emersonfelipesp/netbox-sdk` | Python | SDK+CLI+TUI | Pre-alpha, 16★ | TUI is undescribed/unproven; Python, no static binary |
| Official NetBox web UI / search | — | Web | Baseline | Documented slow; weak search; browser-bound (bad over SSH) |
| NetBox Labs Copilot / MCP / Operator | — | Vendor AI | GA / active | Writes paywalled (Cloud/Enterprise); we're free, self-hosted, human-first |

## How we win (differentiators)

1. **Lookup-first, relationship-first.** Organized around the operator's question ("what owns this IP", "what's on this VLAN", "which port maps here"), not API endpoints. Everyone else is endpoint/CRUD-shaped.
2. **The TUI is the moat.** Search → fuzzy filter → drill device→interface→IP→prefix→VLAN → open/copy. Category-defining; nobody else credibly ships it.
3. **Speed as the headline.** Beat NetBox's own accepted-slow search; market with a ripgrep-style quantified claim ("UI 20–30s vs nbox <1s", screenshotted).
4. **Single static Rust binary** in a Python-dominated field — `brew install` / one-liner, no runtime.
5. **Safe, diff-confirmed writes (v0.2).** Free + self-hosted + RBAC-respecting, shipping before the vendor's "governed writes (soon)" and not paywalled like Copilot.
6. **Excellent IPAM workflows** — next-free-IP/prefix, utilization, a navigable hierarchical prefix tree (uniquely tractable in a terminal). This is our stated identity; make it real.

## The agent angle (stance)

**Differentiate: human-TUI-first + agent-ready — do not fight to be "the NetBox agent tool."** The vendor owns that category (first-party MCP server, Copilot GA Feb 2026, Operator, llms.txt, their own skill files). "For AI agents" is now **table-stakes, not a moat**.

- **Match the checklist cheaply** so we never look dated: versioned JSON envelope, semantic exit codes, structured JSON errors, `--fields`/`--raw` token controls, `--dry-run`, a schema/introspection command, `AGENTS.md` + skill files.
- **Out-execute on safety:** an agent-safe `--read-only` profile + diff-confirmed writes — the thing both the competitor and the official read-only MCP server lack today.
- **Post-1.0, be protocol-agnostic:** `nbox mcp serve` (stdio + HTTP) reusing the same core — positioned as the self-hosted, low-token, write-safe complement to the official read-only server. Don't bet the product on it.

Sources: official MCP server ([github.com/netboxlabs/netbox-mcp-server](https://github.com/netboxlabs/netbox-mcp-server)), Copilot GA + Operator (NetBox Labs blog), agent-CLI patterns (poehnelt.com, Firecrawl MCP-vs-CLI).

## Jobs-to-be-done (priority order)

1. **"What is this IP / device / serial?"** instant resolve *with full context* (site/rack/VRF/VLAN/interface).
2. **Reverse/relationship lookups** — what owns this IP, what's on this VLAN, where is this cabled.
3. **Scriptable JSON** as a first-class citizen (replace the curl+jq status quo).
4. **Client-side filter validation** — NetBox silently ignores unknown query params (returns *everything*); refuse/warn.
5. **Next-free-IP / next-free-prefix** one-liner.
6. **Navigable IPAM** — prefix tree with inline utilization.
7. **Safe, previewable edits** (v0.2).

## Go-to-market / launch playbook

**Distribution is the #1 throttle — clear it before launch.**

1. **Ship install:** `cargo-dist` → GitHub Release binaries (mac Intel/ARM, Linux x86_64/aarch64, Windows) + Homebrew tap + curl one-liner; **reserve `nbox` on crates.io now** (see [RELEASING.md](../RELEASING.md)).
2. **README is the launch** — a VHS-recorded hero GIF: `/edge01` → keyboard drill-down with the speed visible; 3 more short clips (IP→prefix→VLAN; `--json | jq`; `o`/`y`). Demo-first.
3. **Quantified speed claim** with a screenshot (the ripgrep move).
4. **Zero-to-wow < 60s** — `config init` → paste URL+token → `nbox`; point trials at `demo.netbox.dev`.

**Launch sequence (network engineers are allergic to marketing — lead with the artifact, answer as a peer):**

1. Home turf: **NetDev Community Slack `#netbox`**, **NetBox GitHub Discussions**, ask **NetBox Labs to amplify** (they explicitly offer), PR to **awesome-netbox**.
2. Builder communities: **r/networkautomation**, **r/rust**, **This Week in Rust**, **awesome-tuis**.
3. **Network to Code Slack** (largest network-automation community), then **Show HN** (neutral title; reposting later is normal).
4. Longer lead: lightning talks — **NLNOG Day** (CFP ~Jul 20, 2026), **NetBox Evolve 2026** (Oct, the bullseye venue); a genuinely technical blog post that ipspace.net / Kirk Byers would find organically.

## Risks

- **Vendor owns the AI lane** — don't compete head-on there.
- **Market is niche** (single/low-double-digit stars on third-party tools) — real but not huge; the TUI is what makes us the category, not just "another CLI."
- **Name proximity** — `nbox` vs the abandoned `nbx` and "netbox". Mitigated (registries clean), but be consistent and reserve the crate name.

## Sources

Competitor teardown, community pain points, terminal-tool adoption playbook, and agent-CLI/MCP analysis — full source URLs are captured in the research that produced this doc (NetBox GitHub issues/discussions #21196/#12314/#21255/#6489, HN 39274125, NetBox Labs blog, k9s/lazygit/ripgrep adoption write-ups, Firecrawl MCP-vs-CLI, poehnelt.com agent-CLI patterns).
