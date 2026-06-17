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
use rmcp::model::{JsonObject, ProtocolVersion, ServerCapabilities, ServerInfo};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

use crate::domain::detail;
use crate::domain::interface_view::InterfaceView;
use crate::domain::journal_view::JournalView;
use crate::domain::tag_view::TagsView;
use crate::error::NboxError;
use crate::netbox::client::NetBoxClient;
use crate::netbox::search::{SearchFilters, SearchRequest, SearchResult};

/// The read-only NetBox MCP server.
#[derive(Clone)]
pub struct NboxMcp {
    client: Arc<NetBoxClient>,
    tool_router: ToolRouter<Self>,
}

/// The object kinds `nbox_get` (and `nbox_journal`) can resolve.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GetKind {
    Device,
    Ip,
    Prefix,
    Vlan,
    Site,
    Rack,
    Circuit,
    Aggregate,
    Asn,
    IpRange,
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
}

/// Arguments for `nbox_get`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
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

/// `nbox_status` result: connection target plus NetBox/Django/Python versions.
///
/// The version keys mirror the previous `serde_json::json!` shape exactly: they
/// are always present, with `null` when NetBox omits the value (no
/// `skip_serializing_if`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusReport {
    /// The NetBox base URL the server is connected to.
    pub netbox_url: String,
    /// The NetBox application version.
    pub netbox_version: String,
    /// The Django framework version NetBox runs on (`null` if unreported).
    pub django_version: Option<String>,
    /// The Python runtime version NetBox runs on (`null` if unreported).
    pub python_version: Option<String>,
}

/// `nbox_search` result: ranked hits plus any per-endpoint failures.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchReport {
    /// Ranked search hits across devices, sites, IPs, prefixes, VLANs,
    /// circuits, aggregates, ASNs, and IP ranges.
    pub results: Vec<SearchResult>,
    /// Per-endpoint failures (partial result); empty when every endpoint succeeded.
    pub errors: Vec<String>,
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

#[tool_router]
impl NboxMcp {
    /// Build a server bound to a NetBox client.
    pub fn new(client: NetBoxClient) -> Self {
        Self {
            client: Arc::new(client),
            tool_router: Self::tool_router(),
        }
    }

    /// Show NetBox connection and version info. Use this first to confirm
    /// reachability and the NetBox/Django/Python versions. No parameters.
    #[tool(
        name = "nbox_status",
        description = "Show NetBox connection and version info (NetBox/Django/Python versions). Use to confirm reachability before other lookups.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_status(&self) -> Result<Json<StatusReport>, ErrorData> {
        let status = self.client.status().await.map_err(to_mcp_error)?;
        Ok(Json(StatusReport {
            netbox_url: self.client.base_url().as_str().to_string(),
            netbox_version: status.netbox_version,
            django_version: status.django_version,
            python_version: status.python_version,
        }))
    }

    /// Search across devices, sites, IPs, prefixes, VLANs, circuits,
    /// aggregates, ASNs, and IP ranges.
    #[tool(
        name = "nbox_search",
        description = "Search across devices, sites, IP addresses, prefixes, VLANs, circuits, aggregates, ASNs, and IP ranges by free text. Returns ranked hits with kind, display name, and URL. Use this to find an object's exact reference before nbox_get. Optional filters narrow by status/site/tenant/role/tag.",
        annotations(read_only_hint = true)
    )]
    async fn nbox_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<Json<SearchReport>, ErrorData> {
        let outcome = self
            .client
            .search(SearchRequest {
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
                },
            })
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
        description = "Look up a single object and its context. `kind` is one of: device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range. `ref` is the natural reference for that kind (name/slug/ID; CIDR for prefix/aggregate; address for ip; VID or name for vlan; AS number for asn). On an ambiguous reference the error lists the candidates: pass `vrf` for an ip/prefix in several VRFs, or `site`/`group` for a VLAN VID present at several sites.",
        output_schema = output_schema(),
        annotations(read_only_hint = true)
    )]
    async fn nbox_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.get_impl(args).await.map_err(to_mcp_error)
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
        description = "Return recent journal entries (operator notes) for an object, newest first. `kind` and `ref` follow nbox_get; supported kinds are device, ip, prefix, vlan, site, rack, circuit, aggregate, asn, ip_range.",
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
        };
        Ok(Json(value))
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
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Read-only NetBox lookups: search, devices, IPs, prefixes, VLANs, sites, racks, \
             circuits, journals, tags. Use nbox_search to find an object's reference, then \
             nbox_get to fetch it. Nothing is ever written."
                .into(),
        );
        info
    }
}

/// Serve the MCP server over stdio until the client disconnects.
///
/// stdout is reserved for the JSON-RPC stream; this prints nothing else.
pub async fn serve(client: NetBoxClient) -> anyhow::Result<()> {
    let service = NboxMcp::new(client).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
