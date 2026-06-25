//! Typed NetBox capability report + per-surface backend resolution.
//!
//! REST is canonical; GraphQL is an opt-in per-surface accelerator. This module
//! turns a profile's configured [`BackendPreference`] per [`ApiSurface`] plus the
//! live GraphQL schema probe into:
//!   - an [`EffectiveBackend`] the operation routing acts on, and
//!   - an [`ApiRouting`] / [`NetBoxCapabilities`] the `status` surfaces expose.
//!
//! It deliberately summarizes capabilities rather than leaking every
//! introspection detail; feature code still uses the lower-level REST/GraphQL
//! helpers that enforce exact behavior.

use serde::Serialize;

use crate::config::{ApiSurface, BackendPreference};
use crate::netbox::client::NetBoxClient;
use crate::netbox::graphql::GraphqlCapabilities;
use crate::netbox::status::{MIN_MAJOR, MIN_MINOR, Status, meets_minimum};

/// Why GraphQL never backs the search surface — a product rule, not a schema
/// gap. NetBox's GraphQL filtering moved to per-field Strawberry lookups in 4.3
/// and exposes no equivalent to REST's full-text `q` quick-search, so `nbox
/// search` keeps canonical NetBox search semantics by always using REST.
const SEARCH_REST_ONLY_REASON: &str =
    "NetBox GraphQL exposes no REST-equivalent full-text (q) search";

/// The resolved backend for one operation: the configured preference reconciled
/// against the live capability probe. `RestFallback` records *why* a GraphQL
/// preference could not be honored so `status` can explain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveBackend {
    Rest,
    Graphql,
    RestFallback { reason: String },
}

impl EffectiveBackend {
    /// True only when the operation should use GraphQL.
    #[must_use]
    pub fn uses_graphql(&self) -> bool {
        matches!(self, Self::Graphql)
    }

    /// The `rest`/`graphql` label for status output (a fallback reads as `rest`).
    #[must_use]
    pub fn label(&self) -> &'static str {
        if self.uses_graphql() {
            "graphql"
        } else {
            "rest"
        }
    }

    /// The fallback reason, when a GraphQL preference resolved to REST.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::RestFallback { reason } => Some(reason.clone()),
            _ => None,
        }
    }
}

/// Configured-vs-effective backend routing for every surface (`status.api`).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ApiRouting {
    pub search: SurfaceRouting,
    pub vrf: SurfaceRouting,
    pub route_target: SurfaceRouting,
}

/// One surface's routing: what was configured, what is effective, and (on a
/// fallback) why they differ.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SurfaceRouting {
    /// The configured preference (`rest`/`graphql`).
    pub configured: BackendPreference,
    /// The effective backend after capability resolution (`rest`/`graphql`).
    pub effective: String,
    /// Why the effective backend differs from the configured one, if it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Capabilities for the currently connected NetBox instance/profile.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct NetBoxCapabilities {
    /// Version compatibility facts from `/api/status/`.
    pub version: VersionCapabilities,
    /// REST behavior nbox relies on.
    pub rest: RestCapabilities,
    /// GraphQL capability summary. Probed only when a surface prefers GraphQL.
    pub graphql: GraphqlBackendCapabilities,
}

/// NetBox version support facts.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct VersionCapabilities {
    /// Reported NetBox version.
    pub netbox: String,
    /// Minimum NetBox version this build supports.
    pub minimum_supported: String,
    /// Whether the reported version meets the floor.
    pub compatible: bool,
}

/// REST behavior nbox treats as foundational.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RestCapabilities {
    /// REST is always the canonical backend and remains available after status succeeds.
    pub available: bool,
    /// Search fan-out can use REST.
    pub search: bool,
    /// Detail/view lookups use REST.
    pub detail: bool,
    /// Effective page size after profile clamping.
    pub page_size: usize,
    /// Whether device/VM list calls exclude config context by default.
    pub exclude_config_context: bool,
}

/// GraphQL capability summary (surface-aware).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlBackendCapabilities {
    /// Whether this report attempted GraphQL introspection (only when a surface
    /// prefers GraphQL — a pure-REST profile keeps `status` cheap).
    pub probed: bool,
    /// Whether the introspection probe succeeded.
    pub available: bool,
    /// Probe error, when GraphQL was probed but unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Per-surface GraphQL support, when the probe succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surfaces: Option<GraphqlSurfaces>,
}

/// Per-surface GraphQL support.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlSurfaces {
    pub search: SurfaceSupport,
    pub vrf: SurfaceSupport,
    pub route_target: SurfaceSupport,
}

/// Whether a GraphQL surface is usable, recommended, and what (if anything) the
/// schema is missing for it. Version is a hint; the schema probe is truth.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SurfaceSupport {
    /// Whether nbox can run this surface over GraphQL at all.
    pub supported: bool,
    /// Whether GraphQL is the recommended backend for this surface (full coverage).
    pub recommended: bool,
    /// Schema pieces nbox expects for full support that were not found.
    pub missing: Vec<String>,
}

/// GraphQL search support: never. `nbox search` means canonical NetBox search,
/// which GraphQL can't express (see [`SEARCH_REST_ONLY_REASON`]), so the search
/// surface is reported unsupported regardless of schema and always routes to
/// REST. A GraphQL single-POST name/description filter would be a *different*
/// surface (a future `browse`/typeahead), not search.
fn search_support() -> SurfaceSupport {
    SurfaceSupport {
        supported: false,
        recommended: false,
        missing: vec![SEARCH_REST_ONLY_REASON.to_string()],
    }
}

/// True when `list_name` exposes a `key` filter (used for the VRF surface's
/// `vrf_id` requirements).
fn list_has_filter(caps: &GraphqlCapabilities, list_name: &str, key: &str) -> bool {
    let Some(field) = caps.list(list_name) else {
        return false;
    };
    let Some(filter_type) = field.filter_type() else {
        return false;
    };
    caps.filter_shape(filter_type, key).is_some()
}

/// GraphQL VRF support requires `vrf_id` filtering on prefixes and IP addresses
/// (the children bundle). Identity/header resolution stays REST, so `vrf_list`
/// is not required. All-or-nothing - a partial schema falls back to REST with the
/// missing pieces named.
fn vrf_support(caps: &GraphqlCapabilities) -> SurfaceSupport {
    let mut missing = Vec::new();
    if !list_has_filter(caps, "prefix_list", "vrf_id") {
        missing.push("prefix_list.vrf_id".to_string());
    }
    if !list_has_filter(caps, "ip_address_list", "vrf_id") {
        missing.push("ip_address_list.vrf_id".to_string());
    }
    let supported = missing.is_empty();
    SurfaceSupport {
        supported,
        recommended: supported,
        missing,
    }
}

/// GraphQL route-target support requires the route-target list plus `id`
/// filtering (the single filtered selection that carries the importing/exporting
/// VRF relations). All-or-nothing — a partial schema falls back to REST with the
/// missing pieces named. The nested `importing_vrfs`/`exporting_vrfs` fields are
/// standard on NetBox's RouteTargetType across the 4.x line, so the list + id
/// filter is the practical gate (mirroring `vrf_support`).
fn route_target_support(caps: &GraphqlCapabilities) -> SurfaceSupport {
    let mut missing = Vec::new();
    if caps.list("route_target_list").is_none() {
        missing.push("route_target_list".to_string());
    }
    if !list_has_filter(caps, "route_target_list", "id") {
        missing.push("route_target_list.id".to_string());
    }
    let supported = missing.is_empty();
    SurfaceSupport {
        supported,
        recommended: supported,
        missing,
    }
}

fn surface_support(caps: &GraphqlCapabilities, surface: ApiSurface) -> SurfaceSupport {
    match surface {
        ApiSurface::Search => search_support(),
        ApiSurface::Vrf => vrf_support(caps),
        ApiSurface::RouteTarget => route_target_support(caps),
    }
}

impl NetBoxClient {
    /// Resolve a surface's configured preference against the live probe. REST
    /// passes straight through; a GraphQL preference is honored only when the
    /// surface is supported, else it falls back to REST with a reason.
    pub async fn effective_backend(&self, surface: ApiSurface) -> EffectiveBackend {
        if self.api_preference(surface) == BackendPreference::Rest {
            return EffectiveBackend::Rest;
        }
        // Search is REST-canonical by product rule, not a schema gap, so a
        // `graphql` preference falls back without even probing.
        if surface == ApiSurface::Search {
            return EffectiveBackend::RestFallback {
                reason: SEARCH_REST_ONLY_REASON.to_string(),
            };
        }
        match self.graphql_capabilities().await {
            Ok(caps) => {
                let support = surface_support(&caps, surface);
                if support.supported {
                    EffectiveBackend::Graphql
                } else {
                    EffectiveBackend::RestFallback {
                        reason: format!(
                            "GraphQL {} surface unavailable: missing {}",
                            surface.key(),
                            support.missing.join(", ")
                        ),
                    }
                }
            }
            Err(err) => EffectiveBackend::RestFallback {
                reason: format!("GraphQL unavailable: {err:#}"),
            },
        }
    }

    /// Configured-vs-effective routing for every surface (`status.api`).
    pub async fn api_routing(&self) -> ApiRouting {
        ApiRouting {
            search: self.surface_routing(ApiSurface::Search).await,
            vrf: self.surface_routing(ApiSurface::Vrf).await,
            route_target: self.surface_routing(ApiSurface::RouteTarget).await,
        }
    }

    async fn surface_routing(&self, surface: ApiSurface) -> SurfaceRouting {
        let effective = self.effective_backend(surface).await;
        SurfaceRouting {
            configured: self.api_preference(surface),
            effective: effective.label().to_string(),
            reason: effective.reason(),
        }
    }

    /// Build a capability report from an already-fetched status payload.
    ///
    /// REST facts are local/config-derived after `/api/status/` succeeds. GraphQL
    /// introspection is attempted only when a surface prefers GraphQL, so
    /// `nbox status` stays cheap for the default REST profile.
    pub async fn capabilities(&self, status: &Status) -> NetBoxCapabilities {
        let graphql = if self.any_graphql_preferred() {
            match self.graphql_capabilities().await {
                Ok(caps) => GraphqlBackendCapabilities {
                    probed: true,
                    available: true,
                    error: None,
                    surfaces: Some(GraphqlSurfaces {
                        search: search_support(),
                        vrf: vrf_support(&caps),
                        route_target: route_target_support(&caps),
                    }),
                },
                Err(err) => GraphqlBackendCapabilities {
                    probed: true,
                    available: false,
                    error: Some(format!("{err:#}")),
                    surfaces: None,
                },
            }
        } else {
            GraphqlBackendCapabilities {
                probed: false,
                available: false,
                error: None,
                surfaces: None,
            }
        };

        NetBoxCapabilities {
            version: VersionCapabilities {
                netbox: status.netbox_version.clone(),
                minimum_supported: format!("{MIN_MAJOR}.{MIN_MINOR}"),
                compatible: meets_minimum(&status.netbox_version, MIN_MAJOR, MIN_MINOR),
            },
            rest: RestCapabilities {
                available: true,
                search: true,
                detail: true,
                page_size: self.page_size(),
                exclude_config_context: self.exclude_config_context(),
            },
            graphql,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiConfig, ProfileConfig};
    use crate::netbox::client::NetBoxClient;
    use crate::netbox::status::Status;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn status() -> Status {
        Status {
            netbox_version: "4.5.5".into(),
            django_version: None,
            python_version: None,
        }
    }

    fn graphql_profile(server: &MockServer) -> NetBoxClient {
        NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                api: Some(ApiConfig {
                    search: Some(BackendPreference::Graphql),
                    vrf: Some(BackendPreference::Graphql),
                    route_target: Some(BackendPreference::Graphql),
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap()
    }

    async fn mount_graphql_probe(
        server: &MockServer,
        prefix_filter: serde_json::Value,
        ip_filter: serde_json::Value,
        route_target_filter: serde_json::Value,
    ) {
        let list_field = |name: &str, filter: &str| {
            serde_json::json!({
                "name": name,
                "args": [
                    {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": filter}}
                ]
            })
        };
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("__schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {"__schema": {"queryType": {"fields": [
                    list_field("prefix_list", "PrefixFilter"),
                    list_field("ip_address_list", "IPAddressFilter"),
                    list_field("route_target_list", "RouteTargetFilter"),
                ]}}}
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("DeviceFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "prefix": prefix_filter,
                    "ip": ip_filter
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ASNFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "routeTarget": route_target_filter
                }
            })))
            .mount(server)
            .await;
    }

    fn input_with_field(name: &str) -> serde_json::Value {
        serde_json::json!({
            "inputFields": [
                {"name": name, "type": {"kind": "INPUT_OBJECT", "name": "IntegerLookup"}}
            ]
        })
    }

    fn input_without_fields() -> serde_json::Value {
        serde_json::json!({"inputFields": []})
    }

    #[tokio::test]
    async fn rest_profile_reports_local_capabilities_without_graphql_probe() {
        let client = NetBoxClient::new(
            &ProfileConfig {
                url: "http://netbox.example".into(),
                page_size: Some(250),
                exclude_config_context: Some(false),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let caps = client.capabilities(&status()).await;

        assert!(caps.version.compatible);
        assert_eq!(caps.rest.page_size, 250);
        assert!(!caps.rest.exclude_config_context);
        assert!(!caps.graphql.probed);
        assert!(caps.graphql.surfaces.is_none());

        // A pure-REST profile routes both surfaces to REST.
        let routing = client.api_routing().await;
        assert_eq!(routing.search.effective, "rest");
        assert_eq!(routing.vrf.effective, "rest");
    }

    #[tokio::test]
    async fn graphql_profile_reports_probe_error_and_falls_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = graphql_profile(&server);
        let caps = client.capabilities(&status()).await;

        assert!(caps.graphql.probed);
        assert!(!caps.graphql.available);
        assert!(
            caps.graphql
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not found") || e.contains("HTTP 404"))
        );

        // A GraphQL preference with an unreachable schema falls back to REST,
        // surfacing the reason.
        let routing = client.api_routing().await;
        assert_eq!(routing.search.configured, BackendPreference::Graphql);
        assert_eq!(routing.search.effective, "rest");
        assert!(routing.search.reason.is_some());
        assert!(
            !client
                .effective_backend(ApiSurface::Search)
                .await
                .uses_graphql()
        );

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            1,
            "failed GraphQL probe should be cached for this client"
        );
    }

    #[tokio::test]
    async fn graphql_profile_reports_supported_surfaces() {
        let server = MockServer::start().await;
        mount_graphql_probe(
            &server,
            input_with_field("vrf_id"),
            input_with_field("vrf_id"),
            input_with_field("id"),
        )
        .await;

        let client = graphql_profile(&server);
        let caps = client.capabilities(&status()).await;
        let surfaces = caps.graphql.surfaces.expect("surface support");
        assert!(surfaces.vrf.supported);
        assert!(surfaces.vrf.recommended);
        assert!(surfaces.route_target.supported);
        assert!(surfaces.route_target.recommended);
        assert!(!surfaces.search.supported);

        let routing = client.api_routing().await;
        assert_eq!(routing.vrf.effective, "graphql");
        assert_eq!(routing.route_target.effective, "graphql");
    }

    #[tokio::test]
    async fn graphql_profile_names_missing_vrf_filters() {
        let server = MockServer::start().await;
        mount_graphql_probe(
            &server,
            input_without_fields(),
            input_with_field("vrf_id"),
            input_with_field("id"),
        )
        .await;

        let client = graphql_profile(&server);
        let caps = client.capabilities(&status()).await;
        let surfaces = caps.graphql.surfaces.expect("surface support");
        assert!(!surfaces.vrf.supported);
        assert_eq!(surfaces.vrf.missing, vec!["prefix_list.vrf_id"]);

        let effective = client.effective_backend(ApiSurface::Vrf).await;
        assert!(!effective.uses_graphql());
        assert!(
            effective
                .reason()
                .is_some_and(|reason| reason.contains("prefix_list.vrf_id"))
        );
    }

    #[tokio::test]
    async fn graphql_profile_names_missing_route_target_filter() {
        let server = MockServer::start().await;
        mount_graphql_probe(
            &server,
            input_with_field("vrf_id"),
            input_with_field("vrf_id"),
            input_without_fields(),
        )
        .await;

        let client = graphql_profile(&server);
        let caps = client.capabilities(&status()).await;
        let surfaces = caps.graphql.surfaces.expect("surface support");
        assert!(!surfaces.route_target.supported);
        assert_eq!(surfaces.route_target.missing, vec!["route_target_list.id"]);

        let effective = client.effective_backend(ApiSurface::RouteTarget).await;
        assert!(!effective.uses_graphql());
        assert!(
            effective
                .reason()
                .is_some_and(|reason| reason.contains("route_target_list.id"))
        );
    }
}
