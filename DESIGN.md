# nbox — Design Document

> Status: **Original v0.1 design (partly aspirational).**
> Audience: contributors building nbox.
> This is the founding architecture doc. It is the source of truth for *intent* and shapes, but the code has since diverged in places and some modules/sections here are not built yet. For what actually exists today and what's planned, **`ROADMAP.md` is authoritative**. Sections and files that are aspirational are flagged inline below; when in doubt, trust the code and the roadmap.

---

## 1. Project Identity

| Field    | Value                                    |
| -------- | ---------------------------------------- |
| Name     | nbox                                      |
| Repo     | `lance0/nbox`                             |
| Binary   | `nbox`                                    |
| Language | Rust                                     |
| UI       | ratatui + crossterm                      |
| Purpose  | Fast terminal UI and CLI for NetBox      |
| License  | MIT OR Apache-2.0                        |
| Edition  | 2024 (rust-version 1.88)                 |
| NetBox   | 4.2+ (modern polymorphic `scope` model; v2 `Bearer` tokens supported) |

**Tagline**

> nbox is a terminal UI for NetBox, built for fast search, IPAM lookups, device context, and safe operational workflows.

**Positioning**

- Not "NetBox in the terminal."
- Yes "k9s/lazygit-style navigation for NetBox data."

**Integration strategy**

- **REST is the primary integration path.** NetBox's REST API is designed around normal HTTP verbs, JSON objects, list endpoints, detail endpoints, filters, and object IDs. A running instance exposes interactive REST API docs at `/api/schema/swagger-ui/`, which doubles as a development aid and a future schema-discovery tool for nbox.
- **GraphQL is optional and read-only.** NetBox's GraphQL API (`/graphql/`) is explicitly read-only. It is valuable for nested detail views where a single query fetches a device plus its interfaces, IPs, rack, site, and related objects.

---

## 2. Design Goals & Non-Goals

### Primary goals

1. Fast lookup from the shell.
2. Clean interactive TUI.
3. Excellent IPAM workflows.
4. Read-only first.
5. Safe writes later, with diff confirmation.
6. Work well against real enterprise NetBox instances.
7. Feel like Lance network tooling (xfr / ttl family).

### Non-goals for v0

1. Full NetBox CRUD for every model.
2. Replacing the NetBox web UI.
3. Plugin framework.
4. Topology diagrams.
5. Bulk import/export.
6. Custom script runner.
7. Approval workflow engine.

### Product philosophy

nbox exists to answer operational questions quickly:

- What is this IP?
- Where is this device?
- What prefix owns this address?
- What interface has this address?
- What VLAN is this?
- What rack / site / tenant / VRF is this object in?
- Can I open the exact NetBox page?
- Can I copy the exact value I need?

---

## 3. Command Surface

```
nbox                              # launch TUI (no subcommand)
nbox search <query>
nbox device <name-or-id>
nbox ip <address>
nbox prefix <cidr>
nbox site <name-or-slug>
nbox rack <name-or-id>
nbox vlan <vid-or-name>
nbox interface <device> <interface>
nbox open <object-ref>
nbox config init
nbox profile add <name>
nbox profile use <name>
nbox completions <shell>
```

Every shell command works without the TUI, which makes nbox scriptable:

```bash
nbox ip 10.44.208.55
nbox ip 10.44.208.55 --json
nbox device edge01 --json | jq '.primary_ip4.address'
nbox search edge01 --limit 20
```

---

## 4. MVP Scope

### v0.1 — read-only

Ship first:

- Config profiles
- Token auth
- REST client
- Paginated list support
- Device lookup
- IP address lookup
- Prefix lookup
- VLAN lookup
- Site lookup
- Global-ish search across selected endpoints
- Interactive TUI
- Open object in browser
- Copy selected value
- JSON output
- Shell completions

### Writes deferred to v0.2 / v0.3

NetBox supports `PATCH`, and the docs recommend `PATCH` for most updates because it only needs the changed attributes rather than a complete object representation — ideal for safe edit workflows later.

---

## 5. Cargo.toml

nbox mirrors the xfr and ttl dependency posture: ratatui, crossterm, clap, clap_complete, serde, serde_json, toml, dirs, anyhow, tracing, reqwest, tokio, and the same optimized release profile.

```toml
[package]
name = "nbox"
version = "0.1.0"
edition = "2024"
rust-version = "1.88"
description = "Terminal UI and CLI for NetBox"
license = "MIT OR Apache-2.0"
repository = "https://github.com/lance0/nbox"
keywords = ["netbox", "network", "ipam", "tui", "dcim"]
categories = ["command-line-utilities", "network-programming"]

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
futures = "0.3"

# HTTP
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }

# TUI
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }  # async EventStream for the tokio TUI loop
nucleo = "0.5"   # client-side fuzzy ranking for the command palette + in-memory result lists (TUI only)

# CLI
clap = { version = "4", features = ["derive", "env"] }
clap_complete = "4.6"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
toml_edit = "0.25"   # format-preserving writes for `profile add` / config mutation

# Config and paths
dirs = "6"

# Errors and logging
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"

# Terminal helpers
is-terminal = "0.4"
arboard = { version = "3", optional = true }
open = "5"

# IP/network handling
ipnet = "2"

# Time
chrono = { version = "0.4", features = ["serde"] }

# Caching, later
rusqlite = { version = "0.40", features = ["bundled"], optional = true }

# Update notifications, later
update-informer = { version = "1", default-features = false, features = ["github", "ureq", "rustls-tls"], optional = true }

[features]
default = ["clipboard"]
clipboard = ["dep:arboard"]
cache = ["dep:rusqlite"]
updates = ["dep:update-informer"]

[dev-dependencies]
tokio-test = "0.4"
tempfile = "3"
insta = "1"
wiremock = "0.6"

[profile.release]
lto = true
codegen-units = 1
strip = true
```

---

## 6. Repository Layout

Follows the xfr / ttl architecture style: `main.rs`, `cli.rs`, `lib.rs`, a distinct network/API layer, `tui/`, `config.rs`, `prefs.rs`, and docs.

> **Aspirational target — the tree below is the intended shape, not current reality.** As of v0.1.1 several entries are not built: `prefs.rs`, `netbox/graphql.rs`, `netbox/schema.rs`, the `cache/` module, and the `tui/views/` + `tui/widgets/` split (the TUI is currently a flatter `state.rs`/`ui.rs`/`palette.rs`/`fuzzy.rs`). `error.rs`, `output/`, `netbox/`, `domain/`, `util/`, and a `docs/` tree (`ARCHITECTURE.md`, `CONFIG.md`, `FEATURES.md`) exist roughly as shown. See `ROADMAP.md` for what's planned where.

```
nbox/
├── Cargo.toml
├── README.md
├── CHANGELOG.md
├── ROADMAP.md
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── docs/
│   ├── ARCHITECTURE.md
│   ├── FEATURES.md
│   ├── CONFIG.md
│   ├── NETBOX_API.md
│   └── THEMES.md
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── cli.rs
│   ├── config.rs
│   ├── prefs.rs
│   ├── error.rs
│   ├── output/
│   │   ├── mod.rs
│   │   ├── json.rs
│   │   ├── table.rs
│   │   └── plain.rs
│   ├── netbox/
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   ├── auth.rs
│   │   ├── pagination.rs
│   │   ├── endpoints.rs
│   │   ├── query.rs
│   │   ├── search.rs
│   │   ├── graphql.rs
│   │   ├── schema.rs
│   │   └── models/
│   │       ├── mod.rs
│   │       ├── common.rs
│   │       ├── dcim.rs
│   │       ├── ipam.rs
│   │       ├── tenancy.rs
│   │       └── virtualization.rs
│   ├── domain/
│   │   ├── mod.rs
│   │   ├── object_ref.rs
│   │   ├── search_result.rs
│   │   ├── device_view.rs
│   │   ├── ip_view.rs
│   │   ├── prefix_view.rs
│   │   └── actions.rs
│   ├── tui/
│   │   ├── mod.rs
│   │   ├── app.rs
│   │   ├── events.rs
│   │   ├── theme.rs
│   │   ├── layout.rs
│   │   ├── keymap.rs
│   │   ├── state.rs
│   │   ├── views/
│   │   │   ├── mod.rs
│   │   │   ├── home.rs
│   │   │   ├── search.rs
│   │   │   ├── device.rs
│   │   │   ├── ip.rs
│   │   │   ├── prefix.rs
│   │   │   ├── vlan.rs
│   │   │   ├── site.rs
│   │   │   ├── help.rs
│   │   │   └── command.rs
│   │   └── widgets/
│   │       ├── mod.rs
│   │       ├── table.rs
│   │       ├── details.rs
│   │       ├── status.rs
│   │       ├── breadcrumbs.rs
│   │       └── utilization.rs
│   ├── cache/
│   │   ├── mod.rs
│   │   ├── noop.rs
│   │   └── sqlite.rs
│   └── util/
│       ├── mod.rs
│       ├── browser.rs
│       ├── clipboard.rs
│       └── format.rs
└── tests/
    ├── client_tests.rs
    ├── pagination_tests.rs
    └── search_tests.rs
```

---

## 7. Runtime Architecture

```
main.rs
  parse CLI
  load config
  initialize tracing
  create NetBox client
  dispatch command or launch TUI

cli.rs
  clap definitions
  shell completions
  output format flags

config.rs
  profile loading
  token lookup
  TLS settings
  defaults

netbox/
  pure API client
  REST pagination
  endpoint-specific query methods
  optional GraphQL client
  NetBox model structs

domain/
  UI-ready objects
  search result normalization
  object references
  safe action definitions

tui/
  app state
  event loop
  keybindings
  rendering
  command palette

output/
  json/plain/table output for non-TUI mode
```

**Key separation:** API response structs (`netbox/models`) stay separate from TUI view models (`domain/`). NetBox serializers can be "complete" or "brief", and related objects are usually nested brief representations — so the wire model and the view model genuinely differ.

---

## 8. Configuration

### File locations

| OS            | Path                            |
| ------------- | ------------------------------- |
| Linux / macOS | `~/.config/nbox/config.toml`     |
| Windows       | `%APPDATA%\nbox\config.toml`     |

### Example config

```toml
active_profile = "work"

[ui]
theme = "default"
wide = false
confirm_writes = true
open_browser_command = ""

[profiles.work]
url = "https://netbox.example.com"
token_env = "NETBOX_TOKEN"
auth_scheme = "auto"
verify_tls = true
timeout_secs = 15
page_size = 100
exclude_config_context = true

[profiles.lab]
url = "https://netbox.lab.local"
token_env = "NETBOX_LAB_TOKEN"
auth_scheme = "token"
verify_tls = false
timeout_secs = 10
page_size = 100
exclude_config_context = true
```

### Token handling

Do **not** store plaintext tokens by default. Supported auth sources, in priority order:

1. Direct environment override: `NBOX_TOKEN`
2. Environment variable named by `token_env`
3. *Future:* OS keyring
4. *Future:* explicit config token, only with a warning

### Auth scheme handling

NetBox v4.5 added v2 API tokens and recommends them. v2 auth uses `Authorization: Bearer nbt_<key>.<token>`; legacy v1 tokens use `Authorization: Token <token>`.

```rust
pub enum AuthScheme {
    Auto,
    Bearer,
    Token,
}

impl AuthScheme {
    pub fn header_value(&self, token: &str) -> String {
        match self {
            AuthScheme::Bearer => format!("Bearer {token}"),
            AuthScheme::Token => format!("Token {token}"),
            AuthScheme::Auto => {
                if token.starts_with("nbt_") && token.contains('.') {
                    format!("Bearer {token}")
                } else {
                    format!("Token {token}")
                }
            }
        }
    }
}
```

---

## 9. CLI Design

```rust
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "nbox")]
#[command(version)]
#[command(about = "Terminal UI and CLI for NetBox")]
pub struct Cli {
    #[arg(short, long, global = true)]
    pub profile: Option<String>,

    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true)]
    pub no_tui: bool,

    #[arg(long, global = true)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Launch interactive TUI
    Tui,

    /// Search devices, IPs, prefixes, VLANs, racks, sites
    Search {
        query: String,
        #[arg(short, long, default_value_t = 25)]
        limit: usize,
    },

    /// Show a device by name, slug, or ID
    Device { value: String },

    /// Look up an IP address
    Ip { address: String },

    /// Show prefix details and children
    Prefix { cidr: String },

    /// Show a site
    Site { value: String },

    /// Show a rack
    Rack { value: String },

    /// Show a VLAN by VID or name
    Vlan { value: String },

    /// Show an interface on a device
    Interface { device: String, interface: String },

    /// Open a NetBox object in the browser
    Open { object_ref: String },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Manage profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Generate shell completions
    Completions { shell: CompletionShell },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Init,
    Path,
    Show,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Add {
        name: String,
        url: String,
        #[arg(long)]
        token_env: Option<String>,
    },
    Use { name: String },
    List,
    Show { name: Option<String> },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}
```

---

## 10. NetBox API Client

### REST-first

The REST API root lives under `https://<hostname>/api/`, split by application area (DCIM, IPAM, circuits, tenancy, users, virtualization, plugins, …).

```rust
pub enum Endpoint {
    Devices,
    Interfaces,
    Sites,
    Racks,
    IpAddresses,
    Prefixes,
    Vlans,
    Vrfs,
    Tenants,
    VirtualMachines,
}

impl Endpoint {
    pub fn path(&self) -> &'static str {
        match self {
            Endpoint::Devices => "/api/dcim/devices/",
            Endpoint::Interfaces => "/api/dcim/interfaces/",
            Endpoint::Sites => "/api/dcim/sites/",
            Endpoint::Racks => "/api/dcim/racks/",
            Endpoint::IpAddresses => "/api/ipam/ip-addresses/",
            Endpoint::Prefixes => "/api/ipam/prefixes/",
            Endpoint::Vlans => "/api/ipam/vlans/",
            Endpoint::Vrfs => "/api/ipam/vrfs/",
            Endpoint::Tenants => "/api/tenancy/tenants/",
            Endpoint::VirtualMachines => "/api/virtualization/virtual-machines/",
        }
    }
}
```

### Client struct

```rust
#[derive(Clone)]
pub struct NetBoxClient {
    base_url: reqwest::Url,
    token: Option<String>,
    auth_scheme: AuthScheme,
    http: reqwest::Client,
    page_size: usize,
    exclude_config_context: bool,
}

impl NetBoxClient {
    pub fn new(config: ProfileConfig, token: Option<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs.unwrap_or(15)))
            .danger_accept_invalid_certs(!config.verify_tls.unwrap_or(true))
            .build()?;

        Ok(Self {
            base_url: reqwest::Url::parse(&config.url)?,
            token,
            auth_scheme: config.auth_scheme.unwrap_or(AuthScheme::Auto),
            http,
            page_size: config.page_size.unwrap_or(100),
            exclude_config_context: config.exclude_config_context.unwrap_or(true),
        })
    }

    pub async fn get<T>(&self, path: &str, params: &[(&str, String)]) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = self.base_url.join(path.trim_start_matches('/'))?;
        let mut req = self.http.get(url).query(params);

        if let Some(token) = &self.token {
            req = req.header(
                reqwest::header::AUTHORIZATION,
                self.auth_scheme.header_value(token),
            );
        }

        let res = req
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;

        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            anyhow::bail!("NetBox API request failed: {status} {body}");
        }

        Ok(res.json::<T>().await?)
    }
}
```

### Pagination

NetBox list responses include `count`, `next`, `previous`, and `results`. The default page size is governed by `PAGINATE_COUNT` (default 50); clients use `limit` and `offset`. The default maximum page size is 1000 unless reconfigured.

```rust
#[derive(Debug, Deserialize)]
pub struct Page<T> {
    pub count: usize,
    pub next: Option<String>,
    pub previous: Option<String>,
    pub results: Vec<T>,
}

impl NetBoxClient {
    pub async fn list<T>(
        &self,
        endpoint: Endpoint,
        mut params: Vec<(&str, String)>,
    ) -> anyhow::Result<Page<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        params.push(("limit", self.page_size.to_string()));

        if self.exclude_config_context
            && matches!(endpoint, Endpoint::Devices | Endpoint::VirtualMachines)
        {
            params.push(("exclude", "config_context".to_string()));
        }

        self.get(endpoint.path(), &params).await
    }

    pub async fn list_all<T>(
        &self,
        endpoint: Endpoint,
        base_params: Vec<(&str, String)>,
        max: usize,
    ) -> anyhow::Result<Vec<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut out = Vec::new();
        let mut offset = 0;

        loop {
            let mut params = base_params.clone();
            params.push(("limit", self.page_size.to_string()));
            params.push(("offset", offset.to_string()));

            let page: Page<T> = self.get(endpoint.path(), &params).await?;
            let got = page.results.len();
            out.extend(page.results);

            if got == 0 || out.len() >= page.count || out.len() >= max {
                break;
            }
            offset += got;
        }

        out.truncate(max);
        Ok(out)
    }
}
```

> **Performance note:** NetBox recommends excluding rendered config context with `?exclude=config_context` for devices and VMs when it is not needed — large config contexts hurt API performance. nbox does this by default for those endpoints.

### Search strategy

There is no universal global search endpoint to rely on. `nbox search` is a **normalized multi-endpoint search**.

**Primary strategy: the `q` parameter.** Most NetBox endpoints expose a built-in `q=` quick-search (the same fuzzy search the web UI uses), which spans the relevant fields for that object type and survives version drift. nbox uses `q=<query>` as the primary search per endpoint.

**Fallback: explicit field filters.** When `q` is unavailable or too coarse for a given endpoint, fall back to the per-field lookup filters below. These also drive structured filtering in detail views.

v0.1 fallback fields per endpoint:

| Endpoint        | Filters                                            |
| --------------- | ------------------------------------------------- |
| devices         | `name__ic`, `serial__ic`, `asset_tag__ic`         |
| sites           | `name__ic`, `slug__ic`                            |
| racks           | `name__ic`                                        |
| interfaces      | `name__ic`                                        |
| ip-addresses    | `address__ic`, `dns_name__ic`, `description__ic`  |
| prefixes        | `prefix__ic`, `description__ic`                   |
| vlans           | `name__ic`, `vid` exact if numeric                |
| vrfs            | `name__ic`, `rd` exact or contains               |
| tenants         | `name__ic`, `slug__ic`                            |

NetBox filtering supports lookup expressions: case-insensitive contains (`__ic`), exact case-insensitive (`__ie`), numeric comparisons, and ordering via the `ordering` parameter.

```rust
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub kind: ObjectKind,
    pub id: u64,
    pub display: String,
    pub subtitle: Option<String>,
    pub url: String,
    pub path: String,
    pub score: i32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ObjectKind {
    Device,
    Interface,
    Site,
    Rack,
    IpAddress,
    Prefix,
    Vlan,
    Vrf,
    Tenant,
    VirtualMachine,
}
```

Parallel fan-out across endpoints, then merge and rank:

```rust
impl NetBoxClient {
    pub async fn search(&self, req: SearchRequest) -> anyhow::Result<Vec<SearchResult>> {
        let query = req.query.trim().to_string();
        let limit = req.limit;

        let searches = vec![
            self.search_devices(query.clone(), limit),
            self.search_sites(query.clone(), limit),
            self.search_ip_addresses(query.clone(), limit),
            self.search_prefixes(query.clone(), limit),
            self.search_vlans(query.clone(), limit),
        ];

        let results = futures::future::join_all(searches).await;

        let mut merged = Vec::new();
        for result in results {
            match result {
                Ok(mut values) => merged.append(&mut values),
                Err(err) => tracing::warn!("search branch failed: {err:#}"),
            }
        }

        merged.sort_by(|a, b| b.score.cmp(&a.score).then(a.display.cmp(&b.display)));
        merged.truncate(limit);
        Ok(merged)
    }
}
```

### Client-side ranking (TUI only)

NetBox does the *finding* (the `q` query over the network); nbox does the *interactive refining*
on top of whatever came back. Once a result set is in memory, the TUI re-ranks and filters it
with [`nucleo`](https://crates.io/crates/nucleo) — a fast, typo-resistant fuzzy matcher — so the
command palette (`:`), the results list, and recent objects filter instantly as you type, with
**zero network round-trips**.

This is purely a presentation-layer concern: `nucleo` never touches the `netbox/` client and is
not on the path for non-TUI (`--json`/plain) output. At nbox's scale (tens to low-hundreds of
fetched objects) any decent matcher is far faster than the request that produced the data, so the
matcher is about *feel*, not throughput. Lands in Phase 3 alongside the command palette.

> **Future enhancement:** use `OPTIONS` / the OpenAPI schema to validate available filters per NetBox version. The REST `OPTIONS` verb inspects an endpoint and returns supported actions and parameters, so nbox can eventually discover filter/write capability rather than hardcoding it.

### Object references and URL mapping

NetBox objects carry their **API** url (`/api/dcim/devices/1/`), but "open in browser" (`o`)
and any user-facing link needs the **web** url (`/dcim/devices/1/`). This conversion lives in
exactly one place — `domain/object_ref.rs` — so no call site does ad-hoc string surgery.

```rust
/// Canonical handle to a NetBox object: kind + id, with URL derivation centralized here.
#[derive(Debug, Clone)]
pub struct ObjectRef {
    pub kind: ObjectKind,
    pub id: u64,
}

impl ObjectRef {
    /// `/api/dcim/devices/1/` — for further API calls.
    pub fn api_path(&self) -> String { /* kind → endpoint path + id */ }

    /// `https://netbox/dcim/devices/1/` — for the browser / clipboard.
    pub fn web_url(&self, base: &reqwest::Url) -> String { /* strip `/api`, join base */ }

    /// Parse a nested object's `url` field back into an ObjectRef.
    pub fn from_api_url(url: &str) -> Option<Self> { /* … */ }
}
```

The web url is derived by stripping the leading `/api` segment from the API path and joining
it onto the profile's base url. Every `open`/copy-link path goes through `web_url`.

---

## 11. Data Model Layer

Keep response structs **permissive**. NetBox objects often contain nested brief representations, nullable relationships, status objects, tags, custom fields, and timestamps.

### Common types

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BriefObject {
    pub id: u64,
    pub url: Option<String>,
    pub display: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Choice<T> {
    pub value: T,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tag {
    pub id: u64,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomFields(
    #[serde(flatten)]
    pub serde_json::Map<String, serde_json::Value>,
);
```

### Device

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Device {
    pub id: u64,
    pub url: String,
    pub display: Option<String>,
    pub name: String,
    #[serde(default)] pub status: Option<Choice<String>>,
    #[serde(default)] pub role: Option<BriefObject>,
    #[serde(default)] pub device_type: Option<BriefObject>,
    #[serde(default)] pub platform: Option<BriefObject>,
    #[serde(default)] pub site: Option<BriefObject>,
    #[serde(default)] pub rack: Option<BriefObject>,
    #[serde(default)] pub tenant: Option<BriefObject>,
    #[serde(default)] pub primary_ip4: Option<BriefObject>,
    #[serde(default)] pub primary_ip6: Option<BriefObject>,
    #[serde(default)] pub serial: Option<String>,
    #[serde(default)] pub asset_tag: Option<String>,
    #[serde(default)] pub description: Option<String>,
    #[serde(default)] pub tags: Vec<Tag>,
    #[serde(default)] pub custom_fields: serde_json::Value,
}
```

### IP address

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IpAddress {
    pub id: u64,
    pub url: String,
    pub address: String,
    #[serde(default)] pub status: Option<Choice<String>>,
    #[serde(default)] pub role: Option<Choice<String>>,
    #[serde(default)] pub vrf: Option<BriefObject>,
    #[serde(default)] pub tenant: Option<BriefObject>,
    #[serde(default)] pub assigned_object_type: Option<String>,
    #[serde(default)] pub assigned_object_id: Option<u64>,
    #[serde(default)] pub assigned_object: Option<serde_json::Value>,
    #[serde(default)] pub dns_name: Option<String>,
    #[serde(default)] pub description: Option<String>,
    #[serde(default)] pub tags: Vec<Tag>,
    #[serde(default)] pub custom_fields: serde_json::Value,
}
```

### Prefix

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Prefix {
    pub id: u64,
    pub url: String,
    pub prefix: String,
    #[serde(default)] pub status: Option<Choice<String>>,
    #[serde(default)] pub vrf: Option<BriefObject>,
    #[serde(default)] pub tenant: Option<BriefObject>,
    #[serde(default)] pub vlan: Option<BriefObject>,
    #[serde(default)] pub role: Option<BriefObject>,
    // NetBox 4.2+ polymorphic scope (replaced the old `site` field). We target 4.2+, so
    // there is no legacy `site` fallback to carry.
    #[serde(default)] pub scope_type: Option<String>,
    #[serde(default)] pub scope_id: Option<u64>,
    #[serde(default)] pub scope: Option<BriefObject>,
    #[serde(default)] pub description: Option<String>,
    #[serde(default)] pub children: Option<u64>,
    #[serde(default)] pub custom_fields: serde_json::Value,
}
```

---

## 12. TUI Specification

### Default layout

```
┌ nbox ───────────────────────────────────────────────────────────────────────┐
│ profile: work  netbox: https://netbox.example.com  mode: normal            │
├───────────────┬────────────────────────────┬───────────────────────────────┤
│ Navigation    │ Results                    │ Detail                        │
│               │                            │                               │
│ Search        │ > edge01                   │ edge01                        │
│ Devices       │   edge01-oob               │ status: active                │
│ IPAM          │   10.44.208.1/24           │ site: iad1                    │
│ Sites         │   VLAN 208                 │ rack: r12                     │
│ Racks         │                            │ primary IPv4: 10.44.12.9     │
│ VLANs         │                            │                               │
│ Recent        │                            │ Interfaces                    │
│               │                            │ xe-0/0/0  transit-a           │
│               │                            │ xe-0/0/1  transit-b           │
├───────────────┴────────────────────────────┴───────────────────────────────┤
│ / search  Enter open  b back  y copy  o browser  r refresh  ? help  q quit │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Screens

```rust
#[derive(Debug, Clone)]
pub enum Screen {
    Home(HomeState),
    Search(SearchState),
    Device(DeviceState),
    IpAddress(IpState),
    Prefix(PrefixState),
    Vlan(VlanState),
    Site(SiteState),
    Rack(RackState),
    Help,
}
```

### Modes

```rust
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    InsertSearch,
    CommandPalette,
    Loading,
    Error(String),
    ConfirmAction(PendingAction),
}
```

### App state

```rust
pub struct App {
    pub client: NetBoxClient,
    pub config: Config,
    pub prefs: Prefs,

    pub mode: Mode,
    pub screen: Screen,
    pub history: Vec<Screen>,
    pub status: StatusLine,

    pub search_input: String,
    pub command_input: String,

    pub selected: usize,
    pub should_quit: bool,
}
```

### Event loop

Same conceptual pattern as xfr: the TUI receives data over an mpsc channel and key events via terminal event polling, rendering as updates arrive.

```rust
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
    SearchComplete(anyhow::Result<Vec<SearchResult>>),
    DeviceLoaded(anyhow::Result<DeviceView>),
    IpLoaded(anyhow::Result<IpView>),
    PrefixLoaded(anyhow::Result<PrefixView>),
}

pub enum AppCommand {
    Search(String),
    LoadDevice(ObjectRef),
    LoadIp(String),
    LoadPrefix(String),
    OpenBrowser(ObjectRef),
    Copy(String),
}

pub async fn run_tui(client: NetBoxClient, config: Config, prefs: Prefs) -> anyhow::Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(client, config, prefs);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AppEvent>(64);
    spawn_terminal_events(tx.clone());

    while !app.should_quit {
        terminal.draw(|frame| {
            crate::tui::layout::render(frame, &app);
        })?;

        tokio::select! {
            Some(event) = rx.recv() => {
                let commands = app.handle_event(event);
                // Dispatch each command on its own task — never await network here.
                dispatch_commands(commands, &app.client, tx.clone());
            }
        }
    }

    restore_terminal()?;
    Ok(())
}
```

> **TUI commands must be spawned, never awaited in the render loop.** Each `AppCommand`
> runs in its own `tokio::spawn`, posts its result back as an `AppEvent`
> (`SearchComplete`, `DeviceLoaded`, …), and the loop sets `Mode::Loading` meanwhile so
> the UI stays responsive during requests. `NetBoxClient` is `Clone` (reqwest is `Arc`
> internally), so cloning it per task is cheap.
>
> ```rust
> fn dispatch_commands(cmds: Vec<AppCommand>, client: &NetBoxClient, tx: Sender<AppEvent>) {
>     for cmd in cmds {
>         let (client, tx) = (client.clone(), tx.clone());
>         tokio::spawn(async move {
>             let event = match cmd {
>                 AppCommand::Search(q) =>
>                     AppEvent::SearchComplete(client.search(SearchRequest { query: q, limit: 25 }).await),
>                 AppCommand::LoadDevice(r) => AppEvent::DeviceLoaded(client.device_view(r).await),
>                 // … one arm per command
>                 _ => return,
>             };
>             let _ = tx.send(event).await;
>         });
>     }
> }
> ```

### Keybindings

**Global**

| Key            | Action                       |
| -------------- | ---------------------------- |
| `q`            | quit or close modal          |
| `Ctrl+c`       | quit                         |
| `?` / `F1`     | help                         |
| `/`            | search                       |
| `:`            | command palette              |
| `Esc`          | back / close modal           |
| `b`            | back                         |
| `r`            | refresh current screen       |
| `o`            | open selected object in browser |
| `y`            | copy selected field          |
| `Tab`          | next pane                    |
| `Shift+Tab`    | previous pane                |
| `j` / `Down`   | next row                     |
| `k` / `Up`     | previous row                 |
| `g`            | top                          |
| `G`            | bottom                       |
| `Enter`        | open selected object         |

**Search mode**

| Key       | Action                  |
| --------- | ----------------------- |
| `Enter`   | run search / open result |
| `Esc`     | leave search mode       |
| `Ctrl+w`  | delete word             |
| `Ctrl+u`  | clear input             |

**Device screen**

| Key | Action                  |
| --- | ----------------------- |
| `i` | interfaces tab          |
| `p` | IP addresses tab        |
| `c` | cables / connections tab |
| `v` | VLANs tab               |
| `f` | focus field list        |

**Future edit mode**

| Key     | Action            |
| ------- | ----------------- |
| `e`     | edit selected field |
| `d`     | show patch diff   |
| `Enter` | confirm           |
| `Esc`   | cancel            |

---

## 13. Detail Views

### Device view

Data needed: device object, interfaces, IPs assigned to interfaces, primary IPs, rack/site/tenant/role/platform, optional cables (later: inventory items).

REST calls:

```
GET /api/dcim/devices/?name__ie=<name>&exclude=config_context
GET /api/dcim/devices/<id>/?exclude=config_context
GET /api/dcim/interfaces/?device_id=<id>&limit=...
GET /api/ipam/ip-addresses/?assigned_object_type=dcim.interface&assigned_object_id=<interface_id>
```

Better v0.2 GraphQL equivalent — one query for the whole nested view:

```graphql
query DeviceDetail($id: ID!) {
  device(id: $id) {
    id
    name
    status
    site { name slug }
    rack { name }
    role { name }
    platform { name }
    primary_ip4 { address }
    primary_ip6 { address }
    interfaces {
      id
      name
      enabled
      type
      description
      ip_addresses {
        address
        status
      }
    }
  }
}
```

GraphQL fits here because NetBox lets clients request nested fields and provides singular `$OBJECT` and plural `$OBJECT_list` query fields.

### IP view

```bash
nbox ip 10.44.208.55
```

```
10.44.208.55
Status: active
DNS: printer-55.example.com
VRF: blue
Tenant: corp
Assigned: edge01 xe-0/0/1
Parent Prefix: 10.44.208.0/24
VLAN: 208 users
Site: iad1
```

Implementation:

1. Query exact IP address candidates.
2. Query containing parent prefixes.
3. Sort parent prefixes by longest prefix length.
4. Display assigned object if present.

Use `ipnet` locally to compute prefix containment. Do not assume NetBox returns the most specific parent first — explicitly order and verify.

### Prefix view

```bash
nbox prefix 10.44.208.0/24
```

```
10.44.208.0/24
Status: active
VRF: blue
VLAN: 208 users
Site/scope: iad1
Tenant: corp
Children: 4
Description: User access subnet

Child Prefixes
  10.44.208.0/26
  10.44.208.64/26
  10.44.208.128/26
  10.44.208.192/26

IP Addresses
  10.44.208.1/24   edge01 irb.208
  10.44.208.55/24  printer-55
```

### VLAN view

```bash
nbox vlan 208
```

```
VLAN 208 users
Status: active
Group: iad1-campus
Site/scope: iad1
Tenant: corp
Role: user-access

Prefixes
  10.44.208.0/24
  10.45.208.0/24
```

---

## 14. Command Palette

A signature feature.

```
> find edge01
> device edge01
> ip 10.44.208.55
> prefix 10.44.208.0/24
> vlan 208
> open
> copy primary_ip4
> refresh
> profile lab
```

```rust
pub enum PaletteCommand {
    Search(String),
    Device(String),
    Ip(String),
    Prefix(String),
    Vlan(String),
    Open,
    Copy(String),
    Refresh,
    SwitchProfile(String),
}
```

---

## 15. Output Modes

Every non-TUI command supports `plain` and `json`.

```bash
nbox device edge01
```

Plain:

```
edge01
status: active
site: iad1
rack: r12
role: edge-router
platform: junos
primary_ip4: 10.44.12.9/32
```

```bash
nbox device edge01 --json
```

JSON:

```json
{
  "id": 123,
  "name": "edge01",
  "status": "active",
  "site": "iad1",
  "rack": "r12",
  "primary_ip4": "10.44.12.9/32"
}
```

This mirrors the xfr / ttl posture: interactive TUI plus scriptable JSON/plain output, clean stdout for piping, logs to stderr or file.

---

## 16. Safe Writes (v0.2+)

Write support should be explicit and boring.

### Write candidates

```bash
nbox device edge01 set status planned
nbox interface edge01 xe-0/0/1 set description "Transit to ISP-A"
nbox ip 10.44.208.55 reserve --description "Printer"
nbox tag add device edge01 maintenance
```

### Write flow

1. Load current object.
2. Build minimal `PATCH`.
3. Show before/after diff.
4. Require confirmation.
5. Send `PATCH`.
6. Show NetBox response.
7. Log `changelog_message` if provided.

NetBox supports `POST`, `PUT`, `PATCH`, and `DELETE`; `PATCH` only requires the fields being modified. nbox write actions never send whole-object payloads unless required.

```rust
#[derive(Debug, Clone)]
pub struct PendingAction {
    pub object: ObjectRef,
    pub action: ActionKind,
    pub patch: serde_json::Value,
    pub changelog_message: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ActionKind {
    PatchField {
        field: String,
        before: serde_json::Value,
        after: serde_json::Value,
    },
    AddTag { tag: String },
    ReserveIp { address: String, description: Option<String> },
}
```

---

## 17. Error Handling

`anyhow` for CLI/application plumbing; `thiserror` for library-shaped errors.

```rust
#[derive(Debug, thiserror::Error)]
pub enum NetBoxError {
    #[error("authentication failed")]
    Authentication,

    #[error("permission denied")]
    PermissionDenied,

    #[error("object not found: {0}")]
    NotFound(String),

    #[error("multiple objects matched: {0}")]
    Ambiguous(String),

    #[error("NetBox API error {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
}
```

User-facing error style:

```
error: no device matched "edge01"

Try:
  nbox search edge01
```

Avoid dumping raw HTML or huge JSON unless `--debug` is enabled.

---

## 18. Logging

Defaults:

- No logs on stdout.
- Human output stays clean.
- JSON output stays pipe-safe.
- Logs go to stderr or a file.
- **The API token is redacted before any request is logged.** Debug request logging must
  never emit the `Authorization` header value — it is masked (e.g. `Token ****`) at the
  point of logging, so a pasted debug log can't leak credentials.

Config:

```toml
[logging]
level = "info"
file = "~/.config/nbox/nbox.log"
```

Env:

```bash
NBOX_LOG=debug nbox search edge01
RUST_LOG=nbox=debug nbox
```

---

## 19. Themes

Match xfr / ttl: built-in themes, cycle with `t`, persist preference.

Initial themes:

- `default`
- `monochrome`
- `nord`
- `gruvbox`
- `catppuccin`
- `tokyo_night`
- `matrix`

Prefs:

```toml
theme = "default"
```

---

## 20. Documentation Plan

### README.md sections

```
# nbox
Terminal UI and CLI for NetBox.

## Quick Start
## Features
## Installation
## Configuration
## Usage
## TUI Keybindings
## Security
## NetBox Compatibility
## Roadmap
## License
```

### docs/ARCHITECTURE.md

```
# Architecture
## Module Structure
## Data Flow
## NetBox Client
## TUI Event Loop
## Search Pipeline
## Configuration
## Safe Write Design
```

### docs/FEATURES.md

Search · Device lookup · IP lookup · Prefix lookup · VLAN lookup · Site/rack lookup · Command palette · JSON output · Browser open · Clipboard.

---

## 21. Implementation Plan

### Phase 1 — Skeleton

`Cargo.toml`, `main.rs`, `cli.rs`, `config.rs`, `netbox/client.rs`, `netbox/pagination.rs`, `output/json.rs`, basic README.

Deliverable:

```bash
nbox config init
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
nbox search edge01 --json
```

### Phase 2 — Core REST models

Device, Interface, IPAddress, Prefix, VLAN, Site, Rack, BriefObject, Choice, `Page<T>`.

Deliverable:

```bash
nbox device edge01
nbox ip 10.44.208.55
nbox prefix 10.44.208.0/24
nbox vlan 208
```

### Phase 3 — TUI v0

Terminal setup/restore, app state, search screen, detail pane, navigation history, help modal, browser open, copy.

Deliverable:

```bash
nbox
```

### Phase 4 — Polish

Themes, recent objects, better errors, shell completions, install script, GitHub Actions, release builds, Homebrew tap.

### Phase 5 — Safe writes

PATCH engine, diff preview, confirmation modal, `changelog_message` support, write docs.

---

## 22. First Milestone Issue List

- [ ] Create repo `lance0/nbox`
- [ ] Add Cargo.toml metadata and dependencies
- [ ] Add dual MIT/Apache license
- [ ] Add clap CLI skeleton
- [ ] Add config loader
- [ ] Add profile commands
- [ ] Add NetBox auth header support: auto/Bearer/Token
- [ ] Add reqwest client
- [ ] Add paginated `Page<T>`
- [ ] Add Device/IP/Prefix/VLAN/Site/Rack structs
- [ ] Add `search_devices`
- [ ] Add `search_ip_addresses`
- [ ] Add `search_prefixes`
- [ ] Add normalized `SearchResult`
- [ ] Add plain output
- [ ] Add JSON output
- [ ] Add ratatui terminal init/restore
- [ ] Add TUI app state
- [ ] Add search screen
- [ ] Add detail screen
- [ ] Add browser open
- [ ] Add clipboard copy
- [ ] Add docs/ARCHITECTURE.md
- [ ] Add README quick start

---

## 23. First Vertical Slice

Build this exact slice first:

```bash
nbox search edge01
nbox device edge01
nbox ip 10.44.208.55
nbox
```

It proves the whole spine: config, auth, REST client, pagination, model deserialization, plain output, JSON output, TUI event loop, search UX.

**Strongest v0.1 demo:**

1. Open `nbox`.
2. Type `/ edge01`.
3. Select device.
4. Press `Enter`.
5. See device detail.
6. Press `i` for interfaces.
7. Select interface.
8. Press `y` to copy interface name.
9. Press `o` to open object in NetBox.
10. Press `b` to go back.

That is enough to make the project feel real.
