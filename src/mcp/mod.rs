//! Read-only MCP server (stdio transport).
//!
//! A third front-end beside the CLI and TUI. It exposes nbox's existing NetBox
//! read-only lookups as MCP tools, so an agent can drive the same query +
//! domain-view layer the CLI handlers use. Each tool is a thin adapter: it calls
//! the same query helpers, builds the same view model, and returns it as
//! structured JSON.
//!
//! stdout carries the JSON-RPC stream and nothing else — logging goes to stderr
//! (see [`crate::init_logging`]), and the connect path here prints nothing.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    AnnotateAble, JsonObject, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ProtocolVersion, RawResourceTemplate, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, CacheKey};
use crate::domain::detail;
use crate::domain::interface_view::InterfaceView;
use crate::domain::journal_view::JournalView;
use crate::domain::tag_view::TagsView;
use crate::error::NboxError;
use crate::netbox::capabilities::{ApiRouting, NetBoxCapabilities};
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{SearchFilters, SearchRequest, SearchResult};
use crate::netbox::status::AuthCheck;

/// The read-only NetBox MCP server.
#[derive(Clone)]
pub struct NboxMcp {
    client: Arc<NetBoxClient>,
    /// Read cache shared across this long-lived server's tool calls, so a chatty
    /// agent re-reading the same object graph de-dupes within the TTL. Agents can
    /// drop it with `nbox_cache_clear`.
    cache: Cache,
    tool_router: ToolRouter<Self>,
}

/// The object kinds `nbox_get` (and `nbox_journal`) can resolve.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GetKind {
    Device,
    /// IP address. Accepts both `ip` and `ip_address` — the latter is what
    /// `nbox_search` returns as a result `kind`, so an agent can chain
    /// search → get (and `nbox://ip_address/…`) without translating. (Search uses
    /// `ObjectKind::IpAddress` = `"ip_address"`; every other kind already matches
    /// between the two enums, so this is the only alias needed.)
    #[serde(alias = "ip_address")]
    Ip,
    Prefix,
    Vlan,
    Site,
    Rack,
    Circuit,
    Aggregate,
    Asn,
    IpRange,
    Tenant,
    Contact,
    Provider,
    Vm,
    Cluster,
    Vrf,
    RouteTarget,
    /// A MAC address (NetBox 4.2+). Reverse-resolves a MAC to the interface(s)/
    /// device(s) that carry it. The `ref` is the MAC string (any common form is
    /// normalized); a non-MAC is `invalid_params`, >1 interface carrying it is
    /// ambiguous.
    Mac,
}

impl GetKind {
    /// Every kind, in the order the docs and `nbox://{kind}/{ref}` template list
    /// them — the same set `nbox_get` accepts.
    const ALL: [GetKind; 18] = [
        GetKind::Device,
        GetKind::Ip,
        GetKind::Prefix,
        GetKind::Vlan,
        GetKind::Site,
        GetKind::Rack,
        GetKind::Circuit,
        GetKind::Aggregate,
        GetKind::Asn,
        GetKind::IpRange,
        GetKind::Tenant,
        GetKind::Contact,
        GetKind::Provider,
        GetKind::Vm,
        GetKind::Cluster,
        GetKind::Vrf,
        GetKind::RouteTarget,
        GetKind::Mac,
    ];

    /// The `snake_case` slug used in `nbox://{kind}/{ref}` URIs and the `kind`
    /// argument — matches the `#[serde(rename_all = "snake_case")]` spelling.
    fn as_str(self) -> &'static str {
        match self {
            GetKind::Device => "device",
            GetKind::Ip => "ip",
            GetKind::Prefix => "prefix",
            GetKind::Vlan => "vlan",
            GetKind::Site => "site",
            GetKind::Rack => "rack",
            GetKind::Circuit => "circuit",
            GetKind::Aggregate => "aggregate",
            GetKind::Asn => "asn",
            GetKind::IpRange => "ip_range",
            GetKind::Tenant => "tenant",
            GetKind::Contact => "contact",
            GetKind::Provider => "provider",
            GetKind::Vm => "vm",
            GetKind::Cluster => "cluster",
            GetKind::Vrf => "vrf",
            GetKind::RouteTarget => "route_target",
            GetKind::Mac => "mac",
        }
    }

    /// Parse a URI kind slug back to a `GetKind`. Inverse of [`Self::as_str`],
    /// plus the `ip_address` alias for `ip` (the form `nbox_search` emits), so
    /// `nbox://ip_address/…` resolves like `nbox://ip/…`.
    fn from_str(s: &str) -> Option<GetKind> {
        GetKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .or_else(|| (s == "ip_address").then_some(GetKind::Ip))
    }
}

/// Arguments for `nbox_search`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Free-text query (matches names, addresses, serials, descriptions, …).
    pub query: String,
    /// Maximum number of results to return. Defaults to 25.
    pub limit: Option<usize>,
    /// Filter by status, e.g. `active`.
    pub status: Option<String>,
    /// Filter by site (slug, name, or id). Prefixes are matched on site scope.
    /// Mutually exclusive with region/site_group/location.
    pub site: Option<String>,
    /// Filter by region (slug, name, or id). Prefixes are matched on region
    /// scope. Mutually exclusive with site/site_group/location.
    pub region: Option<String>,
    /// Filter by site group (slug, name, or id). Prefixes are matched on
    /// site-group scope. Mutually exclusive with site/region/location.
    pub site_group: Option<String>,
    /// Filter by location (slug, name, or id). Prefixes are matched on location
    /// scope. Mutually exclusive with site/region/site_group.
    pub location: Option<String>,
    /// Filter by tenant slug.
    pub tenant: Option<String>,
    /// Filter by role slug.
    pub role: Option<String>,
    /// Filter by tag slug.
    pub tag: Option<String>,
    /// Filter by VRF (id, RD, or name). Applies to IP and prefix results; other
    /// object kinds carry no VRF and are unaffected.
    pub vrf: Option<String>,
}

/// Arguments for `nbox_get`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct GetArgs {
    /// What kind of object to fetch.
    pub kind: GetKind,
    /// The reference: a name/slug/ID for named objects; a CIDR for prefix and
    /// aggregate; an address for ip; a VID or name for vlan; a start address or
    /// ID for ip_range; the AS number for asn.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Disambiguate by VRF (name, slug, or RD) when an ip/prefix exists in
    /// several VRFs.
    pub vrf: Option<String>,
    /// Disambiguate by site (name or slug) when a VLAN VID exists at several
    /// sites.
    pub site: Option<String>,
    /// Disambiguate by VLAN group (name or slug) when a VID exists in several.
    pub group: Option<String>,
}

/// Arguments for `nbox_get_interface`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InterfaceArgs {
    /// Device name, slug, or numeric ID.
    pub device: String,
    /// Interface name, e.g. `eth0` or `GigabitEthernet0/1`.
    pub interface: String,
}

/// Arguments for `nbox_next_ip`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NextIpArgs {
    /// The prefix (CIDR) to allocate from.
    pub prefix: String,
    /// How many available addresses to return. Defaults to 1.
    pub count: Option<usize>,
    /// Disambiguate the prefix by VRF (name, slug, or RD).
    pub vrf: Option<String>,
}

/// Arguments for `nbox_next_prefix`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NextPrefixArgs {
    /// The parent prefix (CIDR) to allocate from.
    pub prefix: String,
    /// Desired new prefix length, e.g. 26 — returns the first free block of that
    /// size. Omit to list all available free blocks.
    pub length: Option<u8>,
    /// Disambiguate the prefix by VRF (name, slug, or RD).
    pub vrf: Option<String>,
}

/// Arguments for `nbox_journal`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JournalArgs {
    /// What kind of object the entries are attached to.
    pub kind: GetKind,
    /// The object reference (same forms as `nbox_get`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// Maximum number of entries (newest first). Defaults to 20.
    pub limit: Option<usize>,
}

/// Arguments for `nbox_list_tags`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListTagsArgs {
    /// Maximum number of tags to list. Defaults to 200.
    pub limit: Option<usize>,
}

/// `nbox_status` result: connection target, active backend, and versions.
///
/// The version keys mirror the previous `serde_json::json!` shape exactly: they
/// are always present, with `null` when NetBox omits the value (no
/// `skip_serializing_if`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusReport {
    /// The NetBox base URL the server is connected to.
    pub netbox_url: String,
    /// Configured-vs-effective per-surface backend routing (`search`/`vrf`).
    pub api: ApiRouting,
    /// The NetBox application version.
    pub netbox_version: String,
    /// The Django framework version NetBox runs on (`null` if unreported).
    pub django_version: Option<String>,
    /// The Python runtime version NetBox runs on (`null` if unreported).
    pub python_version: Option<String>,
    /// Backend/version capability summary for this connected profile.
    pub capabilities: NetBoxCapabilities,
    /// Credential preflight (`/api/authentication-check/`, NetBox 4.5+): whether
    /// the active profile's token authenticated, and the identity it resolved to.
    /// `unverified` when the endpoint is absent (NetBox < 4.5) or the probe could
    /// not run — distinct from `invalid`, which means the token was rejected.
    pub token: AuthCheck,
}

/// `nbox_search` result: ranked hits plus any per-endpoint failures.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchReport {
    /// Ranked search hits across devices, sites, racks, IPs, prefixes, VLANs,
    /// circuits, aggregates, ASNs, IP ranges, tenants, contacts, providers,
    /// virtual machines, and clusters.
    pub results: Vec<SearchResult>,
    /// Per-endpoint failures (partial result); empty when every endpoint succeeded.
    pub errors: Vec<String>,
}

/// `nbox_cache_clear` result: a small confirmation (a concrete type, so the tool
/// advertises a proper object output schema).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CacheClearReport {
    /// Human-readable confirmation, e.g. "cache cleared".
    pub status: String,
}

/// `nbox_next_ip` / `nbox_next_prefix` result: the resolved prefix plus the
/// available addresses/blocks (rendered as CIDR strings).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AvailableReport {
    /// The prefix (CIDR) the allocation was computed from.
    pub prefix: String,
    /// The available addresses or free child prefixes, as CIDR strings.
    pub available: Vec<String>,
}

/// Map an `anyhow`/`NboxError` chain to an MCP error. Not-found and ambiguous
/// references are caller-fixable, so they become `invalid_params` (with the
/// candidate list in the message); everything else is `internal_error`.
fn to_mcp_error(err: anyhow::Error) -> ErrorData {
    let msg = format!("{err:#}");
    match err.chain().find_map(|e| e.downcast_ref::<NboxError>()) {
        Some(NboxError::NotFound(_) | NboxError::Ambiguous { .. }) => {
            ErrorData::invalid_params(msg, None)
        }
        _ => ErrorData::internal_error(msg, None),
    }
}

/// A permissive `{"type":"object"}` output schema, used only by `nbox_get`.
///
/// `nbox_get` is polymorphic — its return shape depends on `kind`
/// (device/ip/prefix/vlan/…), so it returns `Json<serde_json::Value>` and can't
/// advertise a single concrete schema. The `#[tool]` macro would otherwise
/// derive an output schema from `serde_json::Value`, whose schema has no root
/// `type` and fails rmcp's "outputSchema must be an object" check. This honest,
/// permissive schema is supplied explicitly to satisfy that check. Every
/// type-stable tool instead returns its concrete view type, from which rmcp
/// derives a precise schema via schemars.
fn output_schema() -> Arc<JsonObject> {
    let mut obj = JsonObject::new();
    obj.insert(
        "type".to_string(),
        serde_json::Value::String("object".into()),
    );
    Arc::new(obj)
}

/// A friendly "not found" error, mirroring the CLI's actionable message.
fn not_found(noun: &str, value: &str) -> anyhow::Error {
    NboxError::NotFound(format!(
        "no {noun} matched \"{value}\"\n\nTry: nbox_search with query \"{value}\""
    ))
    .into()
}

/// The URI scheme for nbox resources: `nbox://{kind}/{ref}`.
const RESOURCE_SCHEME: &str = "nbox://";

/// Parse an `nbox://{kind}/{ref}` URI into a [`GetKind`] and its reference.
///
/// `kind` is the `snake_case` slug (see [`GetKind::as_str`]); `ref` is the rest
/// of the path, percent-decoded so a `ref` containing `/` (e.g. a CIDR like
/// `10.0.0.0/24`) round-trips. Returns `invalid_params` for a malformed URI or
/// an unknown kind, mirroring how `nbox_get` reports a caller-fixable mistake.
fn parse_resource_uri(uri: &str) -> Result<(GetKind, String), ErrorData> {
    let bad = |msg: String| ErrorData::invalid_params(msg, None);
    let rest = uri.strip_prefix(RESOURCE_SCHEME).ok_or_else(|| {
        bad(format!(
            "resource URI must start with \"{RESOURCE_SCHEME}\": {uri}"
        ))
    })?;
    let (kind_str, reference) = rest.split_once('/').ok_or_else(|| {
        bad(format!(
            "resource URI must be \"{RESOURCE_SCHEME}{{kind}}/{{ref}}\": {uri}"
        ))
    })?;
    let kind = GetKind::from_str(kind_str).ok_or_else(|| {
        bad(format!(
            "unknown resource kind \"{kind_str}\": expected one of {}",
            GetKind::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    let reference = percent_decode(reference);
    if reference.is_empty() {
        return Err(bad(format!("resource URI has an empty ref: {uri}")));
    }
    Ok((kind, reference))
}

/// The advertised resource templates: a single `nbox://{kind}/{ref}` template
/// covering every [`GetKind`]. A template, not a static list — enumerating every
/// NetBox object would mean walking the whole instance.
fn resource_templates() -> ListResourceTemplatesResult {
    let template = RawResourceTemplate::new("nbox://{kind}/{ref}", "NetBox object")
        .with_title("NetBox object")
        .with_description(
            "Read one NetBox object as JSON. `kind` is one of device, ip, prefix, vlan, \
             site, rack, circuit, aggregate, asn, ip_range, tenant, contact, provider, \
             vm, cluster, vrf, route_target; `ref` is its natural reference (name/slug/ID; CIDR for \
             prefix/aggregate; address for ip; VID or name for vlan; AS number for asn). \
             Percent-encode a `ref` that contains '/'. Same view as the nbox_get tool.",
        )
        .with_mime_type("application/json")
        .no_annotation();
    ListResourceTemplatesResult::with_all_items(vec![template])
}

/// Percent-decode a URI path segment. Tiny and dependency-free: only `%XX`
/// escapes are handled (enough for refs that carry `/`, spaces, or `#`); any
/// malformed escape is left verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tool_router]
impl NboxMcp {
    /// Build a server bound to a NetBox client and a read cache. Pass
    /// `Cache::disabled()` to opt out (e.g. in tests).
    pub fn new(client: NetBoxClient, cache: Cache) -> Self {
        Self {
            client: Arc::new(client),
            cache,
            tool_router: Self::tool_router(),
        }
    }

    /// Show NetBox connection, active backend, and version info. Use this first to confirm
    /// reachability, a valid token (the authenticated user), and the NetBox/Django/Python
    /// versions before other lookups. No parameters.
    #[tool(
        name = "nbox_status",
        description = "Show NetBox connection, active backend, versions, and a token-validity preflight (the authenticated user). Use to confirm reachability and a valid token before other lookups.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_status(&self) -> Result<Json<StatusReport>, ErrorData> {
        let status = self.client.status().await.map_err(to_mcp_error)?;
        let api = self.client.api_routing().await;
        // The credential preflight is independent of the capability probe; overlap
        // them so `nbox_status` costs no extra serial round-trip for the token
        // verdict. Neither returns a `Result`, so a plain `join!` suffices.
        let (capabilities, token) = tokio::join!(
            self.client.capabilities(&status),
            self.client.authentication_check(),
        );
        Ok(Json(StatusReport {
            netbox_url: self.client.base_url().as_str().to_string(),
            api,
            netbox_version: status.netbox_version,
            django_version: status.django_version,
            python_version: status.python_version,
            capabilities,
            token,
        }))
    }

    /// Search across devices, sites, racks, IPs, prefixes, VLANs, circuits,
    /// aggregates, ASNs, and IP ranges.
    #[tool(
        name = "nbox_search",
        description = "Search across devices, sites, racks, IP addresses, prefixes, VLANs, circuits, aggregates, ASNs, IP ranges, tenants, contacts, providers, virtual machines, clusters, VRFs, and route targets by free text. Returns ranked hits with kind, display name, and URL. Use this to find an object's exact reference before nbox_get. Optional filters narrow by status/site/tenant/role/tag; vrf (id|rd|name) narrows IP and prefix results.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<Json<SearchReport>, ErrorData> {
        let outcome = Box::pin(self.client.search(SearchRequest {
            query: args.query,
            limit: args.limit.unwrap_or(25),
            filters: SearchFilters {
                status: args.status,
                site: args.site,
                region: args.region,
                site_group: args.site_group,
                location: args.location,
                tenant: args.tenant,
                role: args.role,
                tag: args.tag,
                vrf: args.vrf,
            },
        }))
        .await
        .map_err(to_mcp_error)?;

        // Fail-closed reporting: surface partial-failure endpoints alongside the
        // results so the agent can decide whether the set is trustworthy.
        Ok(Json(SearchReport {
            results: outcome.results,
            errors: outcome.errors,
        }))
    }

    /// Look up a single NetBox object by kind and reference.
    // Polymorphic: the return shape varies by `kind`, so this stays
    // `Json<serde_json::Value>` with the permissive object schema rather than a
    // single concrete type (a oneOf over ~10 view types is out of scope).
    #[tool(
        name = "nbox_get",
        description = "Look up a single object and its context. `kind` is one of: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range, tenant, contact, provider, vm, cluster, vrf, route_target. `ref` is the natural reference for that kind (name/slug/ID; CIDR for prefix/aggregate; address for ip; VID or name for vlan; AS number for asn; slug/name/ID for tenant; name/ID for contact; slug/name/ID for provider; name/ID for vm and cluster). On an ambiguous reference the error lists the candidates: pass `vrf` for an ip/prefix in several VRFs, or `site`/`group` for a VLAN VID present at several sites.",
        output_schema = output_schema(),
        annotations(read_only_hint = true)
    )]
    async fn nbox_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.get_cached(args).await.map_err(to_mcp_error)
    }

    /// Drop nbox's local read cache so the next reads fetch fresh from NetBox.
    /// Read-only with respect to NetBox — it only clears cached copies held in
    /// this server process; use it after data changed in NetBox out-of-band.
    #[tool(
        name = "nbox_cache_clear",
        description = "Clear nbox's local read cache so the next lookups fetch fresh from NetBox. Use this after data changed in NetBox out-of-band and you need the current state before the cache TTL expires. Safe and read-only with respect to NetBox — it only drops cached copies held in this server process.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn nbox_cache_clear(&self) -> Result<Json<CacheClearReport>, ErrorData> {
        self.cache.clear_all();
        Ok(Json(CacheClearReport {
            status: "cache cleared".to_string(),
        }))
    }

    /// Show one interface on a device, with its addresses and cable-path trace.
    #[tool(
        name = "nbox_get_interface",
        description = "Show one interface on a device: its config, assigned IP addresses, and the cable-path trace (what it connects to). Resolve the device by name, slug, or ID.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_get_interface(
        &self,
        Parameters(args): Parameters<InterfaceArgs>,
    ) -> Result<Json<InterfaceView>, ErrorData> {
        let view =
            detail::interface_view_by_ref(&self.client, &args.device, &args.interface, &not_found)
                .await
                .map_err(to_mcp_error)?;
        Ok(Json(view))
    }

    /// The next available IP address(es) within a prefix.
    #[tool(
        name = "nbox_next_ip",
        description = "Return the next available IP address(es) within a prefix (read-only — nothing is reserved). Pass `count` for several; `vrf` to disambiguate a prefix present in multiple VRFs.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_next_ip(
        &self,
        Parameters(args): Parameters<NextIpArgs>,
    ) -> Result<Json<AvailableReport>, ErrorData> {
        let count = args.count.unwrap_or(1);
        let p = self
            .resolve_prefix(&args.prefix, args.vrf.as_deref())
            .await
            .map_err(to_mcp_error)?;
        let available = self
            .client
            .prefix_available_ips(p.id, count)
            .await
            .map_err(to_mcp_error)?;
        let addresses: Vec<String> = available
            .into_iter()
            .take(count)
            .map(|a| a.address)
            .collect();
        Ok(Json(AvailableReport {
            prefix: p.prefix,
            available: addresses,
        }))
    }

    /// The available (free) prefix(es) within a prefix.
    #[tool(
        name = "nbox_next_prefix",
        description = "Return available (free) child prefixes within a prefix. With `length` (e.g. 26) returns the first free block of that size; without it, lists all free blocks. Pass `vrf` to disambiguate. Read-only — nothing is reserved.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_next_prefix(
        &self,
        Parameters(args): Parameters<NextPrefixArgs>,
    ) -> Result<Json<AvailableReport>, ErrorData> {
        let p = self
            .resolve_prefix(&args.prefix, args.vrf.as_deref())
            .await
            .map_err(to_mcp_error)?;
        let free = self
            .client
            .prefix_available_prefixes(p.id)
            .await
            .map_err(to_mcp_error)?;
        let available: Vec<String> = match args.length {
            Some(len) => crate::first_subnet_of_length(&free, len)
                .into_iter()
                .collect(),
            None => free.into_iter().map(|f| f.prefix).collect(),
        };
        Ok(Json(AvailableReport {
            prefix: p.prefix,
            available,
        }))
    }

    /// Recent journal entries for an object.
    #[tool(
        name = "nbox_journal",
        description = "Return recent journal entries (operator notes) for an object, newest first. `kind` and `ref` follow nbox_get; supported kinds are device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range, tenant, contact, provider, vm, cluster, vrf, route_target.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_journal(
        &self,
        Parameters(args): Parameters<JournalArgs>,
    ) -> Result<Json<JournalView>, ErrorData> {
        let limit = args.limit.unwrap_or(20);
        let (content_type, id) = self
            .resolve_content_type_id(args.kind, &args.reference)
            .await
            .map_err(to_mcp_error)?;
        let entries = self
            .client
            .journal_entries(content_type, id, limit)
            .await
            .map_err(to_mcp_error)?;
        Ok(Json(JournalView::from_models(entries)))
    }

    /// List the tags defined in NetBox.
    #[tool(
        name = "nbox_list_tags",
        description = "List the tags defined in NetBox (name, slug, color, usage count). Useful for discovering valid `tag` filter values for nbox_search.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_list_tags(
        &self,
        Parameters(args): Parameters<ListTagsArgs>,
    ) -> Result<Json<TagsView>, ErrorData> {
        let tags = self
            .client
            .tags(args.limit.unwrap_or(200))
            .await
            .map_err(to_mcp_error)?;
        Ok(Json(TagsView::from_models(tags)))
    }
}

impl NboxMcp {
    /// `nbox_get` through the read cache: serve a within-TTL copy if present, else
    /// fetch via [`get_impl`](Self::get_impl) and store it. Single-flighted, so an
    /// agent firing several reads of the same object collapses to one fetch. The
    /// key folds in the disambiguators so the same CIDR in two VRFs caches apart.
    /// A not-found/ambiguous error still propagates (nothing is cached for it).
    async fn get_cached(&self, args: GetArgs) -> anyhow::Result<Json<serde_json::Value>> {
        let scope = format!(
            "vrf={};site={};group={}",
            args.vrf.as_deref().unwrap_or(""),
            args.site.as_deref().unwrap_or(""),
            args.group.as_deref().unwrap_or(""),
        );
        let key = CacheKey::object(args.kind.as_str(), &args.reference, &scope);
        let cached = self
            .cache
            .get_or_fetch(&key, || async {
                let Json(value) = self.get_impl(args.clone()).await?;
                Ok(value)
            })
            .await?;
        Ok(Json(cached.value))
    }

    /// Dispatch `nbox_get` to the matching shared resolver + view builder.
    /// Returns the JSON view, or a typed error (not-found/ambiguous map to
    /// invalid_params at the tool boundary). The fetch + view-build path is the
    /// same one the CLI handlers use (see [`crate::domain::detail`]); `not_found`
    /// supplies the MCP-flavored "use nbox_search" message.
    async fn get_impl(&self, args: GetArgs) -> anyhow::Result<Json<serde_json::Value>> {
        let c = &self.client;
        let r = args.reference.as_str();
        let value = match args.kind {
            GetKind::Device => {
                serde_json::to_value(detail::device_detail_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Ip => serde_json::to_value(
                detail::ip_view_by_ref(c, r, args.vrf.as_deref(), &not_found).await?,
            )?,
            GetKind::Prefix => serde_json::to_value(
                detail::prefix_view_by_ref(c, r, args.vrf.as_deref(), &not_found).await?,
            )?,
            GetKind::Vlan => serde_json::to_value(
                detail::vlan_view_by_ref(
                    c,
                    r,
                    args.site.as_deref(),
                    args.group.as_deref(),
                    &not_found,
                )
                .await?,
            )?,
            GetKind::Site => {
                serde_json::to_value(detail::site_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Rack => {
                serde_json::to_value(detail::rack_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Circuit => {
                serde_json::to_value(detail::circuit_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Aggregate => {
                serde_json::to_value(detail::aggregate_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Asn => {
                let asn_num: u32 = r.parse().map_err(|_| {
                    NboxError::NotFound(format!("\"{r}\" is not a valid AS number"))
                })?;
                serde_json::to_value(detail::asn_view_by_ref(c, asn_num, r, &not_found).await?)?
            }
            GetKind::IpRange => {
                serde_json::to_value(detail::ip_range_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Tenant => {
                serde_json::to_value(detail::tenant_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Contact => {
                serde_json::to_value(detail::contact_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Provider => {
                serde_json::to_value(detail::provider_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Vm => serde_json::to_value(detail::vm_view_by_ref(c, r, &not_found).await?)?,
            GetKind::Cluster => {
                serde_json::to_value(detail::cluster_view_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Vrf => {
                serde_json::to_value(detail::vrf_detail_by_ref(c, r, &not_found).await?)?
            }
            GetKind::RouteTarget => {
                serde_json::to_value(detail::route_target_detail_by_ref(c, r, &not_found).await?)?
            }
            GetKind::Mac => {
                let mac = crate::mac::normalize(r).ok_or_else(|| {
                    NboxError::Usage(format!(
                        "invalid MAC address \"{r}\" — try aa:bb:cc:dd:ee:ff"
                    ))
                })?;
                serde_json::to_value(detail::mac_view_by_ref(c, &mac, r, &not_found).await?)?
            }
        };
        Ok(Json(value))
    }

    /// Read an `nbox://{kind}/{ref}` resource: parse the URI, resolve through
    /// the same shared view layer as `nbox_get`, and return the object's JSON
    /// view as a single text content. Disambiguators (`vrf`/`site`/`group`) have
    /// no place in a flat URI, so an ambiguous `ref` surfaces its candidate list
    /// as an `invalid_params` error — the caller can then use `nbox_get`.
    async fn read_resource_impl(&self, uri: &str) -> Result<ReadResourceResult, ErrorData> {
        let (kind, reference) = parse_resource_uri(uri)?;
        let Json(value) = self
            .get_impl(GetArgs {
                kind,
                reference,
                vrf: None,
                site: None,
                group: None,
            })
            .await
            .map_err(to_mcp_error)?;
        // Pretty-print so a host that renders the resource shows readable JSON;
        // the bytes are the same view model the tool returns.
        let text = serde_json::to_string_pretty(&value).map_err(|e| {
            ErrorData::internal_error(format!("serialize resource view: {e}"), None)
        })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
    }

    /// Resolve a CIDR to a single prefix, scoped by an optional VRF reference.
    async fn resolve_prefix(
        &self,
        cidr: &str,
        vrf: Option<&str>,
    ) -> anyhow::Result<crate::netbox::models::ipam::Prefix> {
        detail::resolve_prefix(&self.client, cidr, vrf, &not_found).await
    }

    /// Resolve a `<kind> <ref>` to the object's dotted content type and ID, for
    /// the journal lookup. Delegates to the CLI's [`crate::resolve_content_type_id`]
    /// — the single source of truth for the journal-able kind set — so MCP and
    /// CLI can't drift. The only translation is mapping the MCP `GetKind` enum
    /// (snake_case, e.g. `ip_range`) to the CLI kind spelling (`ip-range`); the
    /// CLI resolver itself parses the asn ref to a `u32`.
    async fn resolve_content_type_id(
        &self,
        kind: GetKind,
        value: &str,
    ) -> anyhow::Result<(&'static str, u64)> {
        let cli_kind = match kind {
            GetKind::Device => "device",
            GetKind::Ip => "ip",
            GetKind::Prefix => "prefix",
            GetKind::Vlan => "vlan",
            GetKind::Site => "site",
            GetKind::Rack => "rack",
            GetKind::Circuit => "circuit",
            GetKind::Aggregate => "aggregate",
            GetKind::Asn => "asn",
            GetKind::IpRange => "ip-range",
            GetKind::Tenant => "tenant",
            GetKind::Contact => "contact",
            GetKind::Provider => "provider",
            GetKind::Vm => "vm",
            GetKind::Cluster => "cluster",
            GetKind::Vrf => "vrf",
            GetKind::RouteTarget => "route-target",
            GetKind::Mac => "mac",
        };
        crate::resolve_content_type_id(&self.client, cli_kind, value).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NboxMcp {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive], so it can't be
        // built with a struct literal from here — start from the default and
        // override the fields we care about.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.instructions = Some(
            "Read-only NetBox lookups: search, devices, IPs, prefixes, VLANs, sites, racks, \
             circuits, journals, tags. Use nbox_search to find an object's reference, then \
             nbox_get to fetch it. Objects are also exposed as nbox://{kind}/{ref} resources \
             (same view as nbox_get). Nothing is ever written."
                .into(),
        );
        info
    }

    // Resources mirror the read path: a single `nbox://{kind}/{ref}` template
    // routes through the same view layer as `nbox_get`, so hosts that browse or
    // attach resources get object context without a separate tool call. There is
    // no static list to enumerate (that would mean walking all of NetBox), so
    // `list_resources` stays empty and the template carries the shape. These
    // sit alongside the tool methods the `#[tool_handler]` macro generates — the
    // macro only emits tool/get_info methods, so there is no conflict.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::default())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(resource_templates())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_resource_impl(&request.uri).await
    }
}

/// Serve the MCP server over stdio until the client disconnects.
///
/// stdout is reserved for the JSON-RPC stream; this prints nothing else.
pub async fn serve(client: NetBoxClient, cache: Cache) -> anyhow::Result<()> {
    let service = NboxMcp::new(client, cache).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Opt-in HTTP transport (`nbox serve --http`), behind the `http` feature. The
/// same [`NboxMcp`] backs it; see [`http::serve_http`]. [`oidc`] is the OAuth 2.1
/// resource-server layer applied to `/mcp` when `--oidc-issuer` is configured;
/// [`audit`] is the v1 ops layer (structured audit log + per-caller rate limit).
#[cfg(feature = "http")]
pub mod audit;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "http")]
pub mod oidc;
#[cfg(feature = "http")]
pub use http::{OidcArgs, ServeOptions, serve_http};

#[cfg(test)]
mod tests;
