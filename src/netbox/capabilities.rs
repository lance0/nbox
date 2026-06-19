//! Typed NetBox capability report.
//!
//! This is the shared place for version/backend facts that user-facing surfaces
//! need to expose. It deliberately summarizes capabilities rather than leaking
//! every introspection detail; feature code should still use the lower-level
//! REST/GraphQL helpers that enforce exact behavior.

use serde::Serialize;

use crate::config::BackendKind;
use crate::netbox::client::NetBoxClient;
use crate::netbox::graphql::{FilterShape, GraphqlCapabilities};
use crate::netbox::status::{MIN_MAJOR, MIN_MINOR, Status, meets_minimum};

const SEARCH_LISTS: [&str; 14] = [
    "device_list",
    "site_list",
    "ip_address_list",
    "prefix_list",
    "vlan_list",
    "circuit_list",
    "aggregate_list",
    "asn_list",
    "ip_range_list",
    "tenant_list",
    "contact_list",
    "provider_list",
    "virtual_machine_list",
    "cluster_list",
];

/// Capabilities for the currently connected NetBox instance/profile.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct NetBoxCapabilities {
    /// The active read backend for this profile.
    pub backend: BackendKind,
    /// Version compatibility facts from `/api/status/`.
    pub version: VersionCapabilities,
    /// REST behavior nbox relies on.
    pub rest: RestCapabilities,
    /// GraphQL search capability summary. Probed only when `backend=graphql`.
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
    /// REST is always the primary backend and remains available after status succeeds.
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

/// GraphQL search capability summary.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlBackendCapabilities {
    /// Whether this profile selected the GraphQL backend.
    pub configured: bool,
    /// Whether this report attempted GraphQL introspection.
    pub probed: bool,
    /// Whether the introspection probe succeeded.
    pub available: bool,
    /// Search list-field summary when GraphQL was available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<GraphqlSearchCapabilities>,
    /// Filter-shape summary for drift-prone filters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<GraphqlFilterCapabilities>,
    /// Probe error, when GraphQL was configured but unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// GraphQL search list support.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlSearchCapabilities {
    /// Number of nbox search list fields found in the schema.
    pub lists_found: usize,
    /// Number of found search list fields that support pagination.
    pub paginated_lists: usize,
    /// Search list fields nbox expects but the schema did not expose.
    pub missing_lists: Vec<String>,
}

/// GraphQL filter-shape summary for compatibility-sensitive filters.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlFilterCapabilities {
    /// Prefix/cluster polymorphic scope type filter shape.
    pub scope_type: Option<GraphqlFilterInfo>,
    /// Prefix/cluster polymorphic scope id filter shape.
    pub scope_id: Option<GraphqlFilterInfo>,
    /// Device/VM site id filter shape.
    pub site_id: Option<GraphqlFilterInfo>,
    /// Prefix/IP VRF id filter shape.
    pub vrf_id: Option<GraphqlFilterInfo>,
    /// Tree-node scope filters such as location/region/site-group IDs.
    pub tree_node_scope_ids: bool,
}

/// One GraphQL filter field's high-level shape.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct GraphqlFilterInfo {
    /// GraphQL input shape category: `scalar`, `list`, or `lookup`.
    pub shape: &'static str,
    /// GraphQL named type when introspection reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_type: Option<String>,
}

impl NetBoxClient {
    /// Build a capability report from an already-fetched status payload.
    ///
    /// REST facts are local/config-derived after `/api/status/` succeeds.
    /// GraphQL introspection is attempted only for profiles that explicitly opt
    /// into the GraphQL backend, so `nbox status` stays cheap for the default
    /// REST profile.
    pub async fn capabilities(&self, status: &Status) -> NetBoxCapabilities {
        let graphql = if self.backend() == BackendKind::Graphql {
            match self.graphql_capabilities().await {
                Ok(caps) => GraphqlBackendCapabilities::from_caps(true, caps),
                Err(err) => GraphqlBackendCapabilities {
                    configured: true,
                    probed: true,
                    available: false,
                    search: None,
                    filters: None,
                    error: Some(format!("{err:#}")),
                },
            }
        } else {
            GraphqlBackendCapabilities {
                configured: false,
                probed: false,
                available: false,
                search: None,
                filters: None,
                error: None,
            }
        };

        NetBoxCapabilities {
            backend: self.backend(),
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

impl GraphqlBackendCapabilities {
    fn from_caps(configured: bool, caps: GraphqlCapabilities) -> Self {
        Self {
            configured,
            probed: true,
            available: true,
            search: Some(GraphqlSearchCapabilities::from_caps(&caps)),
            filters: Some(GraphqlFilterCapabilities::from_caps(&caps)),
            error: None,
        }
    }
}

impl GraphqlSearchCapabilities {
    fn from_caps(caps: &GraphqlCapabilities) -> Self {
        let mut lists_found = 0;
        let mut paginated_lists = 0;
        let mut missing_lists = Vec::new();

        for list in SEARCH_LISTS {
            match caps.list(list) {
                Some(field) => {
                    lists_found += 1;
                    if field.has_pagination() {
                        paginated_lists += 1;
                    }
                }
                None => missing_lists.push(list.to_string()),
            }
        }

        Self {
            lists_found,
            paginated_lists,
            missing_lists,
        }
    }
}

impl GraphqlFilterCapabilities {
    fn from_caps(caps: &GraphqlCapabilities) -> Self {
        Self {
            scope_type: filter_info(caps, "prefix_list", "scope_type"),
            scope_id: filter_info(caps, "prefix_list", "scope_id"),
            site_id: filter_info(caps, "device_list", "site_id"),
            vrf_id: filter_info(caps, "prefix_list", "vrf_id"),
            tree_node_scope_ids: ["region_id", "site_group_id", "location_id"]
                .iter()
                .any(|key| {
                    filter_info(caps, "device_list", key)
                        .and_then(|info| info.named_type)
                        .as_deref()
                        == Some("TreeNodeFilter")
                }),
        }
    }
}

fn filter_info(
    caps: &GraphqlCapabilities,
    list_name: &str,
    key: &str,
) -> Option<GraphqlFilterInfo> {
    let filter_type = caps.list(list_name)?.filter_type()?;
    Some(GraphqlFilterInfo {
        shape: caps.filter_shape(filter_type, key)?.as_str(),
        named_type: caps.filter_named_type(filter_type, key).map(str::to_string),
    })
}

impl FilterShape {
    fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::List => "list",
            Self::Lookup => "lookup",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;
    use crate::netbox::client::NetBoxClient;
    use crate::netbox::status::Status;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        let caps = client
            .capabilities(&Status {
                netbox_version: "4.5.5".into(),
                django_version: None,
                python_version: None,
            })
            .await;

        assert_eq!(caps.backend, BackendKind::Rest);
        assert!(caps.version.compatible);
        assert_eq!(caps.rest.page_size, 250);
        assert!(!caps.rest.exclude_config_context);
        assert!(!caps.graphql.configured);
        assert!(!caps.graphql.probed);
    }

    #[tokio::test]
    async fn graphql_profile_reports_probe_error_without_failing_capabilities() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                backend: Some(BackendKind::Graphql),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let caps = client
            .capabilities(&Status {
                netbox_version: "4.5.5".into(),
                django_version: None,
                python_version: None,
            })
            .await;

        assert_eq!(caps.backend, BackendKind::Graphql);
        assert!(caps.graphql.configured);
        assert!(caps.graphql.probed);
        assert!(!caps.graphql.available);
        assert!(
            caps.graphql
                .error
                .as_deref()
                .is_some_and(|e| { e.contains("not found") || e.contains("HTTP 404") })
        );
    }
}
