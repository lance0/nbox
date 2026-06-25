//! GraphQL schema capability helpers.
//!
//! NetBox's GraphQL surface has changed across the 4.x line: 4.2 list fields
//! have no pagination argument, 4.3 adds offset pagination, and 4.5 requires
//! lookup objects for ID/enum filters. Rather than branching solely on the
//! version string, nbox probes the schema and shapes filters from the advertised
//! input types.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::domain::route_target_view::VrfRef;
use crate::domain::util::non_empty;
use crate::netbox::client::{MAX_PAGE_SIZE, NetBoxClient};
use crate::netbox::models::common::Choice;
use crate::netbox::models::ipam::{IpAddress, Prefix};

const QUERY_INTROSPECTION: &str = r"
query {
  __schema {
    queryType {
      fields {
        name
        args {
          name
          type { kind name ofType { kind name ofType { kind name ofType { kind name } } } }
        }
      }
    }
  }
}
";

const FILTER_INTROSPECTION_A: &str = r#"
query {
  device: __type(name: "DeviceFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  site: __type(name: "SiteFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  ip: __type(name: "IPAddressFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  prefix: __type(name: "PrefixFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  vlan: __type(name: "VLANFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  circuit: __type(name: "CircuitFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  aggregate: __type(name: "AggregateFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
}
"#;

const FILTER_INTROSPECTION_B: &str = r#"
query {
  asn: __type(name: "ASNFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  ipRange: __type(name: "IPRangeFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  tenant: __type(name: "TenantFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  contact: __type(name: "ContactFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  provider: __type(name: "ProviderFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  virtualMachine: __type(name: "VirtualMachineFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  cluster: __type(name: "ClusterFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
  routeTarget: __type(name: "RouteTargetFilter") { inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } }
}
"#;

#[derive(Debug, Clone, Default)]
pub struct GraphqlCapabilities {
    list_fields: HashMap<String, ListField>,
    filters: HashMap<String, HashMap<String, FilterField>>,
}

#[derive(Debug, Clone)]
pub struct ListField {
    filter_type: Option<String>,
    pagination_arg: Option<String>,
}

impl ListField {
    #[must_use]
    pub fn filter_type(&self) -> Option<&str> {
        self.filter_type.as_deref()
    }

    #[must_use]
    pub fn has_pagination(&self) -> bool {
        self.pagination_arg.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterShape {
    Scalar,
    List,
    Lookup,
}

impl GraphqlCapabilities {
    #[must_use]
    pub fn list(&self, name: &str) -> Option<&ListField> {
        self.list_fields.get(name)
    }

    #[must_use]
    pub fn filter_shape(&self, filter_type: &str, name: &str) -> Option<FilterShape> {
        self.filters
            .get(filter_type)
            .and_then(|fields| fields.get(name).map(|field| field.shape))
    }

    #[must_use]
    pub fn filter_named_type(&self, filter_type: &str, name: &str) -> Option<&str> {
        self.filters
            .get(filter_type)?
            .get(name)?
            .named_type
            .as_deref()
    }

    /// Shape a filter value for the current NetBox schema.
    ///
    /// Pre-4.5 schemas often expose IDs/enums as scalars or string lists, while
    /// 4.5+ exposes lookup input objects. For lookup inputs, `exact` preserves the
    /// old equality semantics.
    #[must_use]
    pub fn filter_value(&self, filter_type: &str, name: &str, value: Value) -> Option<Value> {
        let field = self.filters.get(filter_type)?.get(name)?;
        if name == "scope_type" && field.named_type.as_deref() == Some("ContentTypeFilter") {
            let Value::String(content_type) = value else {
                return None;
            };
            let (app_label, model) = content_type.split_once('.')?;
            return Some(json!({
                "app_label": { "exact": app_label },
                "model": { "exact": model },
            }));
        }
        if field.named_type.as_deref() == Some("TreeNodeFilter") {
            return Some(json!({
                "id": coerce_list_item(value),
                "match_type": "EXACT",
            }));
        }
        let value = normalize_filter_value(name, value, field);
        match field.shape {
            FilterShape::Scalar => Some(value),
            FilterShape::List => Some(Value::Array(vec![coerce_list_item(value)])),
            FilterShape::Lookup => Some(json!({ "exact": value })),
        }
    }
}

#[derive(Debug, Clone)]
struct FilterField {
    shape: FilterShape,
    named_type: Option<String>,
}

impl NetBoxClient {
    pub async fn graphql_capabilities(&self) -> Result<GraphqlCapabilities> {
        let capabilities = self
            .graphql_capability_cache()
            .get_or_init(|| async {
                self.load_graphql_capabilities()
                    .await
                    .map_err(|err| format!("{err:#}"))
            })
            .await;
        match capabilities {
            Ok(capabilities) => Ok(capabilities.clone()),
            Err(err) => Err(anyhow!(err.clone())),
        }
    }

    async fn load_graphql_capabilities(&self) -> Result<GraphqlCapabilities> {
        let schema: SchemaResponse = self.graphql(QUERY_INTROSPECTION, json!({})).await?;
        let first: FilterResponse = self.graphql(FILTER_INTROSPECTION_A, json!({})).await?;
        let second: FilterResponse = self.graphql(FILTER_INTROSPECTION_B, json!({})).await?;
        Ok(GraphqlCapabilities::from_parts(
            schema.schema,
            [first, second],
        ))
    }

    /// Fetch a VRF's prefixes + IP addresses in a single GraphQL query (the VRF
    /// detail "bundle"), normalized into the REST wire models so the downstream
    /// `VrfDetail` build is byte-identical to the REST path. The caller resolves
    /// the VRF id and its header over REST first (identity stays canonical); this
    /// fetches only the children. GraphQL/transport errors propagate to the detail
    /// builder, which retries the same detail over REST rather than degrading to
    /// empty data.
    pub(crate) async fn graphql_vrf_bundle(
        &self,
        vrf_id: u64,
        limit: usize,
    ) -> Result<(Vec<Prefix>, Vec<IpAddress>)> {
        let caps = self.graphql_capabilities().await?;
        let mut prefix_filters = Map::new();
        let mut ip_filters = Map::new();
        gql_add_required_filter(
            &caps,
            "prefix_list",
            &mut prefix_filters,
            "vrf_id",
            json!(vrf_id),
        )?;
        gql_add_required_filter(
            &caps,
            "ip_address_list",
            &mut ip_filters,
            "vrf_id",
            json!(vrf_id),
        )?;
        let prefix_type = caps
            .list("prefix_list")
            .and_then(|f| f.filter_type())
            .context("GraphQL schema is missing the prefix_list filter type")?;
        let ip_type = caps
            .list("ip_address_list")
            .and_then(|f| f.filter_type())
            .context("GraphQL schema is missing the ip_address_list filter type")?;
        let page = limit.clamp(1, MAX_PAGE_SIZE);
        let prefix_pag = gql_pagination(&caps, "prefix_list", page);
        let ip_pag = gql_pagination(&caps, "ip_address_list", page);

        let query = format!(
            "query($pf: {prefix_type}, $if: {ip_type}) {{ \
             prefix_list(filters: $pf{prefix_pag}) {{ id prefix _depth status description }} \
             ip_address_list(filters: $if{ip_pag}) {{ id address status dns_name description }} }}"
        );
        let bundle: VrfBundleResponse = self
            .graphql(
                &query,
                json!({ "pf": Value::Object(prefix_filters), "if": Value::Object(ip_filters) }),
            )
            .await?;

        let prefixes = bundle
            .prefix_list
            .into_iter()
            .filter_map(GqlVrfPrefix::into_prefix)
            .collect();
        let addresses = bundle
            .ip_address_list
            .into_iter()
            .filter_map(GqlVrfAddress::into_ip)
            .collect();
        Ok((prefixes, addresses))
    }

    /// Fetch a route target's importing/exporting VRFs in a single GraphQL query
    /// (the route-target detail "bundle"), normalized into the same [`VrfRef`]
    /// shape the REST path produces so the downstream `RouteTargetDetail` build is
    /// byte-identical. The caller resolves the route target's identity + header
    /// over REST first (identity stays canonical); this fetches only the relation
    /// graph. GraphQL/transport errors propagate to the detail builder, which
    /// retries the same detail over REST rather than degrading to empty data.
    ///
    /// A route target carries its VRF relations on both sides
    /// (`importing_vrfs`/`exporting_vrfs`), so one filtered `route_target_list`
    /// selection replaces the REST path's two `vrfs` list calls.
    pub(crate) async fn graphql_route_target_bundle(
        &self,
        route_target_id: u64,
    ) -> Result<(Vec<VrfRef>, Vec<VrfRef>)> {
        let caps = self.graphql_capabilities().await?;
        let mut filters = Map::new();
        gql_add_required_filter(
            &caps,
            "route_target_list",
            &mut filters,
            "id",
            json!(route_target_id),
        )?;
        let filter_type = caps
            .list("route_target_list")
            .and_then(|f| f.filter_type())
            .context("GraphQL schema is missing the route_target_list filter type")?;

        let query = format!(
            "query($rt: {filter_type}) {{ \
             route_target_list(filters: $rt) {{ \
             importing_vrfs {{ id name rd }} exporting_vrfs {{ id name rd }} }} }}"
        );
        let bundle: RouteTargetBundleResponse = self
            .graphql(&query, json!({ "rt": Value::Object(filters) }))
            .await?;

        // The filter targets a single route target by id; take the one row. The id
        // was already resolved over REST, so a missing GraphQL row is a surprise
        // (schema/permission/consistency skew) — degrade like the REST path on an
        // isolated target (an empty relation graph), but leave a breadcrumb.
        let Some(r) = bundle.route_target_list.into_iter().next() else {
            tracing::debug!(
                route_target_id,
                "GraphQL route_target bundle returned no row for a REST-resolved id; relation graph empty"
            );
            return Ok((Vec::new(), Vec::new()));
        };
        Ok((
            gql_vrf_refs(r.importing_vrfs),
            gql_vrf_refs(r.exporting_vrfs),
        ))
    }
}

/// Reshape the GraphQL nested VRF rows into the REST path's [`VrfRef`] form:
/// numeric ids (GraphQL gives strings) and empty RDs dropped. Input order is
/// preserved — the canonical `(name, rd)` order is applied once, downstream, by
/// [`sort_vrf_refs`] (which runs on *both* backends), so the two produce identical
/// output without this function repeating that sort.
fn gql_vrf_refs(rows: Vec<GqlRtVrf>) -> Vec<VrfRef> {
    rows.into_iter().filter_map(GqlRtVrf::into_ref).collect()
}

/// Pagination clause for a GraphQL list field, empty when it isn't paginated.
fn gql_pagination(caps: &GraphqlCapabilities, list_name: &str, limit: usize) -> String {
    match caps.list(list_name) {
        Some(field) if field.has_pagination() => {
            format!(", pagination: {{offset: 0, limit: {limit}}}")
        }
        _ => String::new(),
    }
}

/// Insert a schema-shaped value for `key` into `filters` when the list's filter
/// type exposes it; returns whether it was added.
fn gql_add_filter(
    caps: &GraphqlCapabilities,
    list_name: &str,
    filters: &mut Map<String, Value>,
    key: &str,
    value: Value,
) -> bool {
    let Some(filter_type) = caps.list(list_name).and_then(|field| field.filter_type()) else {
        return false;
    };
    let Some(value) = caps.filter_value(filter_type, key, value) else {
        return false;
    };
    filters.insert(key.into(), value);
    true
}

fn gql_add_required_filter(
    caps: &GraphqlCapabilities,
    list_name: &str,
    filters: &mut Map<String, Value>,
    key: &str,
    value: Value,
) -> Result<()> {
    if gql_add_filter(caps, list_name, filters, key, value) {
        return Ok(());
    }
    let Some(filter_type) = caps.list(list_name).and_then(|field| field.filter_type()) else {
        bail!("GraphQL schema is missing the {list_name} filter type");
    };
    if caps.filter_shape(filter_type, key).is_none() {
        bail!("GraphQL {filter_type} is missing required {key} filter");
    }
    bail!("GraphQL {filter_type}.{key} filter rejected the required value");
}

/// The combined VRF-bundle response. GraphQL ids are strings, `status` is a
/// plain enum string, and prefixes carry `_depth`; each row deserializes into a
/// typed struct, then maps into the REST wire model so REST and GraphQL converge
/// on one downstream view-build path.
#[derive(Debug, Deserialize)]
struct VrfBundleResponse {
    #[serde(default)]
    prefix_list: Vec<GqlVrfPrefix>,
    #[serde(default)]
    ip_address_list: Vec<GqlVrfAddress>,
}

#[derive(Debug, Deserialize)]
struct GqlVrfPrefix {
    id: String,
    prefix: String,
    #[serde(rename = "_depth")]
    depth: Option<u64>,
    status: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GqlVrfAddress {
    id: String,
    address: String,
    status: Option<String>,
    dns_name: Option<String>,
    description: Option<String>,
}

/// The route-target-bundle response. The single filtered `route_target_list` row
/// carries the importing/exporting VRF relations; each VRF row deserializes into
/// a typed struct, then maps into the domain [`VrfRef`] (string id → numeric,
/// empty RD dropped) so REST and GraphQL converge on one downstream view-build.
#[derive(Debug, Deserialize)]
struct RouteTargetBundleResponse {
    #[serde(default)]
    route_target_list: Vec<GqlRouteTarget>,
}

#[derive(Debug, Deserialize)]
struct GqlRouteTarget {
    #[serde(default)]
    importing_vrfs: Vec<GqlRtVrf>,
    #[serde(default)]
    exporting_vrfs: Vec<GqlRtVrf>,
}

#[derive(Debug, Deserialize)]
struct GqlRtVrf {
    id: String,
    name: String,
    rd: Option<String>,
}

impl GqlRtVrf {
    /// Map into the navigable [`VrfRef`]. A non-numeric id drops the row rather
    /// than failing the bundle; an empty RD is normalized to `None`, exactly as
    /// the REST [`VrfRef::from_model`] path does.
    fn into_ref(self) -> Option<VrfRef> {
        Some(VrfRef {
            id: self.id.parse().ok()?,
            name: self.name,
            rd: self.rd.and_then(non_empty),
        })
    }
}

/// A GraphQL plain-string `status` (`"active"`) becomes the REST `Choice` shape
/// (`{value,label}`, both set to the string).
fn gql_status(status: Option<String>) -> Option<Choice<String>> {
    status.map(|s| Choice {
        label: s.clone(),
        value: s,
    })
}

impl GqlVrfPrefix {
    /// Map into the REST [`Prefix`] (only the fields the VRF tree needs; the rest
    /// default). A non-numeric id drops the row rather than failing the bundle.
    fn into_prefix(self) -> Option<Prefix> {
        Some(Prefix {
            id: self.id.parse().ok()?,
            prefix: self.prefix,
            status: gql_status(self.status),
            description: self.description,
            depth: self.depth,
            ..Prefix::default()
        })
    }
}

impl GqlVrfAddress {
    /// Map into the REST [`IpAddress`]. A non-numeric id drops the row.
    fn into_ip(self) -> Option<IpAddress> {
        Some(IpAddress {
            id: self.id.parse().ok()?,
            address: self.address,
            status: gql_status(self.status),
            dns_name: self.dns_name,
            description: self.description,
            ..IpAddress::default()
        })
    }
}

fn coerce_list_item(value: Value) -> Value {
    match value {
        Value::Number(n) => Value::String(n.to_string()),
        other => other,
    }
}

fn normalize_filter_value(name: &str, value: Value, field: &FilterField) -> Value {
    if name != "status" || field.named_type.as_deref() == Some("String") {
        return value;
    }
    let Value::String(status) = value else {
        return value;
    };
    if status.starts_with("STATUS_") {
        Value::String(status)
    } else {
        Value::String(format!(
            "STATUS_{}",
            status
                .chars()
                .map(|c| if c == '-' {
                    '_'
                } else {
                    c.to_ascii_uppercase()
                })
                .collect::<String>()
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaResponse {
    #[serde(rename = "__schema")]
    schema: Schema,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterResponse {
    device: Option<InputType>,
    site: Option<InputType>,
    ip: Option<InputType>,
    prefix: Option<InputType>,
    vlan: Option<InputType>,
    circuit: Option<InputType>,
    aggregate: Option<InputType>,
    asn: Option<InputType>,
    ip_range: Option<InputType>,
    tenant: Option<InputType>,
    contact: Option<InputType>,
    provider: Option<InputType>,
    virtual_machine: Option<InputType>,
    cluster: Option<InputType>,
    route_target: Option<InputType>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Schema {
    query_type: QueryType,
}

#[derive(Debug, Deserialize)]
struct QueryType {
    fields: Vec<QueryField>,
}

#[derive(Debug, Deserialize)]
struct QueryField {
    name: String,
    args: Vec<QueryArg>,
}

#[derive(Debug, Deserialize)]
struct QueryArg {
    name: String,
    #[serde(rename = "type")]
    type_: TypeRef,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputType {
    input_fields: Option<Vec<InputField>>,
}

#[derive(Debug, Deserialize)]
struct InputField {
    name: String,
    #[serde(rename = "type")]
    type_: TypeRef,
}

#[derive(Debug, Clone, Deserialize)]
struct TypeRef {
    kind: String,
    name: Option<String>,
    #[serde(rename = "ofType")]
    of_type: Option<Box<TypeRef>>,
}

impl TypeRef {
    fn named(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or_else(|| self.of_type.as_ref()?.named())
    }

    fn outer_kind(&self) -> &str {
        if self.kind == "NON_NULL" {
            self.of_type
                .as_ref()
                .map_or(self.kind.as_str(), |inner| inner.outer_kind())
        } else {
            &self.kind
        }
    }

    fn shape(&self) -> FilterShape {
        match self.outer_kind() {
            "LIST" => FilterShape::List,
            "INPUT_OBJECT" => FilterShape::Lookup,
            _ => FilterShape::Scalar,
        }
    }
}

impl GraphqlCapabilities {
    fn from_parts<const N: usize>(schema: Schema, batches: [FilterResponse; N]) -> Self {
        let mut list_fields = HashMap::new();
        for field in schema.query_type.fields {
            if !field.name.ends_with("_list") {
                continue;
            }
            let filter_type = field
                .args
                .iter()
                .find(|arg| arg.name == "filters")
                .and_then(|arg| arg.type_.named())
                .map(str::to_string);
            let pagination_arg = field
                .args
                .iter()
                .find(|arg| arg.name == "pagination")
                .map(|arg| arg.name.clone());
            list_fields.insert(
                field.name,
                ListField {
                    filter_type,
                    pagination_arg,
                },
            );
        }

        let mut filters = HashMap::new();
        for batch in batches {
            for (name, input) in batch.inputs() {
                let Some(fields) = input.input_fields else {
                    tracing::warn!(
                        filter_type = name,
                        "NetBox GraphQL filter type has no inputFields; filtered searches for this branch may be skipped"
                    );
                    continue;
                };
                filters.insert(
                    name.to_string(),
                    fields
                        .into_iter()
                        .map(|f| {
                            (
                                f.name,
                                FilterField {
                                    shape: f.type_.shape(),
                                    named_type: f.type_.named().map(str::to_string),
                                },
                            )
                        })
                        .collect::<HashMap<_, _>>(),
                );
            }
        }

        Self {
            list_fields,
            filters,
        }
    }
}

impl FilterResponse {
    fn inputs(self) -> Vec<(&'static str, InputType)> {
        let mut inputs = Vec::new();
        if let Some(input) = self.device {
            inputs.push(("DeviceFilter", input));
        }
        if let Some(input) = self.site {
            inputs.push(("SiteFilter", input));
        }
        if let Some(input) = self.ip {
            inputs.push(("IPAddressFilter", input));
        }
        if let Some(input) = self.prefix {
            inputs.push(("PrefixFilter", input));
        }
        if let Some(input) = self.vlan {
            inputs.push(("VLANFilter", input));
        }
        if let Some(input) = self.circuit {
            inputs.push(("CircuitFilter", input));
        }
        if let Some(input) = self.aggregate {
            inputs.push(("AggregateFilter", input));
        }
        if let Some(input) = self.asn {
            inputs.push(("ASNFilter", input));
        }
        if let Some(input) = self.ip_range {
            inputs.push(("IPRangeFilter", input));
        }
        if let Some(input) = self.tenant {
            inputs.push(("TenantFilter", input));
        }
        if let Some(input) = self.contact {
            inputs.push(("ContactFilter", input));
        }
        if let Some(input) = self.provider {
            inputs.push(("ProviderFilter", input));
        }
        if let Some(input) = self.virtual_machine {
            inputs.push(("VirtualMachineFilter", input));
        }
        if let Some(input) = self.cluster {
            inputs.push(("ClusterFilter", input));
        }
        if let Some(input) = self.route_target {
            inputs.push(("RouteTargetFilter", input));
        }
        inputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_filter_values_match_schema_shape() {
        let mut caps = GraphqlCapabilities::default();
        caps.filters.insert(
            "DeviceFilter".into(),
            HashMap::from([
                (
                    "q".into(),
                    FilterField {
                        shape: FilterShape::Scalar,
                        named_type: Some("String".into()),
                    },
                ),
                (
                    "site_id".into(),
                    FilterField {
                        shape: FilterShape::List,
                        named_type: None,
                    },
                ),
                (
                    "id".into(),
                    FilterField {
                        shape: FilterShape::Lookup,
                        named_type: Some("IDFilterLookup".into()),
                    },
                ),
            ]),
        );

        assert_eq!(
            caps.filter_value("DeviceFilter", "q", json!("edge")),
            Some(json!("edge"))
        );
        assert_eq!(
            caps.filter_value("DeviceFilter", "site_id", json!(1)),
            Some(json!(["1"]))
        );
        assert_eq!(
            caps.filter_value("DeviceFilter", "id", json!(1)),
            Some(json!({ "exact": 1 }))
        );
    }

    #[test]
    fn content_type_filter_splits_scope_type_into_app_and_model() {
        let mut caps = GraphqlCapabilities::default();
        caps.filters.insert(
            "PrefixFilter".into(),
            HashMap::from([(
                "scope_type".into(),
                FilterField {
                    shape: FilterShape::Lookup,
                    named_type: Some("ContentTypeFilter".into()),
                },
            )]),
        );

        assert_eq!(
            caps.filter_value("PrefixFilter", "scope_type", json!("dcim.sitegroup")),
            Some(json!({
                "app_label": { "exact": "dcim" },
                "model": { "exact": "sitegroup" }
            }))
        );
    }

    #[test]
    fn tree_node_filter_shapes_id_with_exact_match_type() {
        let mut caps = GraphqlCapabilities::default();
        caps.filters.insert(
            "DeviceFilter".into(),
            HashMap::from([(
                "location_id".into(),
                FilterField {
                    shape: FilterShape::Lookup,
                    named_type: Some("TreeNodeFilter".into()),
                },
            )]),
        );

        assert_eq!(
            caps.filter_value("DeviceFilter", "location_id", json!(7)),
            Some(json!({ "id": "7", "match_type": "EXACT" }))
        );
    }

    #[test]
    fn required_filter_errors_instead_of_querying_unscoped_data() {
        let mut caps = GraphqlCapabilities::default();
        caps.list_fields.insert(
            "prefix_list".into(),
            ListField {
                filter_type: Some("PrefixFilter".into()),
                pagination_arg: None,
            },
        );
        caps.filters.insert("PrefixFilter".into(), HashMap::new());
        let mut filters = Map::new();

        let err = gql_add_required_filter(&caps, "prefix_list", &mut filters, "vrf_id", json!(42))
            .expect_err("missing required filter should error");

        assert!(
            format!("{err:#}").contains("PrefixFilter is missing required vrf_id filter"),
            "unexpected error: {err:#}"
        );
        assert!(filters.is_empty());
    }

    #[test]
    fn pagination_clause_tracks_schema_support() {
        let mut caps = GraphqlCapabilities::default();
        caps.list_fields.insert(
            "old_list".into(),
            ListField {
                filter_type: Some("OldFilter".into()),
                pagination_arg: None,
            },
        );
        caps.list_fields.insert(
            "new_list".into(),
            ListField {
                filter_type: Some("NewFilter".into()),
                pagination_arg: Some("pagination".into()),
            },
        );

        assert_eq!(gql_pagination(&caps, "old_list", 200), "");
        assert_eq!(
            gql_pagination(&caps, "new_list", 200),
            ", pagination: {offset: 0, limit: 200}"
        );
    }

    #[test]
    fn status_filter_keeps_legacy_string_but_normalizes_enum_schema() {
        let mut caps = GraphqlCapabilities::default();
        caps.filters.insert(
            "LegacyFilter".into(),
            HashMap::from([(
                "status".into(),
                FilterField {
                    shape: FilterShape::Scalar,
                    named_type: Some("String".into()),
                },
            )]),
        );
        caps.filters.insert(
            "EnumFilter".into(),
            HashMap::from([(
                "status".into(),
                FilterField {
                    shape: FilterShape::Lookup,
                    named_type: Some("StatusFilterLookup".into()),
                },
            )]),
        );

        assert_eq!(
            caps.filter_value("LegacyFilter", "status", json!("active")),
            Some(json!("active"))
        );
        assert_eq!(
            caps.filter_value("EnumFilter", "status", json!("active")),
            Some(json!({ "exact": "STATUS_ACTIVE" }))
        );
    }

    #[test]
    fn gql_vrf_prefix_maps_status_id_and_depth() {
        // GraphQL gives string id, plain-string status, and `_depth`; the wire
        // model wants numeric id, a Choice{value,label} status, and `depth`.
        let row: GqlVrfPrefix = serde_json::from_value(json!({
            "id": "34", "prefix": "10.20.0.0/16", "_depth": 0,
            "status": "container", "description": "supernet"
        }))
        .unwrap();
        let p = row.into_prefix().expect("prefix");
        assert_eq!(p.id, 34);
        assert_eq!(p.prefix, "10.20.0.0/16");
        assert_eq!(p.depth, Some(0));
        assert_eq!(
            p.status.as_ref().map(|c| c.value.as_str()),
            Some("container")
        );
        assert_eq!(p.description.as_deref(), Some("supernet"));

        // A null status (GraphQL can omit it) stays None, not an error.
        let row: GqlVrfPrefix = serde_json::from_value(
            json!({"id": "35", "prefix": "10.20.1.0/24", "_depth": 1, "status": null}),
        )
        .unwrap();
        let p = row.into_prefix().expect("prefix");
        assert!(p.status.is_none());
        assert_eq!(p.depth, Some(1));
    }

    #[test]
    fn gql_vrf_address_maps_fields() {
        let row: GqlVrfAddress = serde_json::from_value(json!({
            "id": "6", "address": "10.20.1.10/24", "status": "active",
            "dns_name": "web-01.customer"
        }))
        .unwrap();
        let ip = row.into_ip().expect("ip");
        assert_eq!(ip.id, 6);
        assert_eq!(ip.address, "10.20.1.10/24");
        assert_eq!(ip.status.as_ref().map(|c| c.value.as_str()), Some("active"));
        assert_eq!(ip.dns_name.as_deref(), Some("web-01.customer"));
    }

    #[test]
    fn gql_vrf_prefix_rejects_nonnumeric_id() {
        // A non-numeric id (shouldn't happen, but be defensive) drops the row
        // rather than panicking.
        let row: GqlVrfPrefix =
            serde_json::from_value(json!({"id": "abc", "prefix": "10.0.0.0/8"})).unwrap();
        assert!(row.into_prefix().is_none());
    }

    #[tokio::test]
    async fn graphql_vrf_bundle_fetches_scoped_children_in_one_query() {
        use crate::config::{ApiConfig, BackendPreference, ProfileConfig};
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Schema probe: prefix_list + ip_address_list, each with a filters arg
        // and offset pagination.
        let list_field = |name: &str, filter: &str| {
            json!({
                "name": name,
                "args": [
                    {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": filter}},
                    {"name": "pagination", "type": {"kind": "INPUT_OBJECT", "name": "PaginationInput"}}
                ]
            })
        };
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("__schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"__schema": {"queryType": {"fields": [
                    list_field("prefix_list", "PrefixFilter"),
                    list_field("ip_address_list", "IPAddressFilter"),
                ]}}}
            })))
            .mount(&server)
            .await;

        // Filter probe: batch A carries PrefixFilter + IPAddressFilter; both
        // expose a vrf_id lookup so the bundle can scope its children.
        let vrf_id_field =
            json!({"name": "vrf_id", "type": {"kind": "INPUT_OBJECT", "name": "IntegerLookup"}});
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("DeviceFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "prefix": {"inputFields": [vrf_id_field.clone()]},
                    "ip": {"inputFields": [vrf_id_field]}
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ASNFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;

        // The bundle itself: one POST carrying both list selections.
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ip_address_list(filters"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "prefix_list": [
                        {"id": "1", "prefix": "10.50.0.0/16", "_depth": 0, "status": "container", "description": "supernet"},
                        {"id": "2", "prefix": "10.50.1.0/24", "_depth": 1, "status": "active", "description": ""}
                    ],
                    "ip_address_list": [
                        {"id": "9", "address": "10.50.1.1/24", "status": "active", "dns_name": "gw.customer", "description": ""}
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                api: Some(ApiConfig {
                    search: None,
                    vrf: Some(BackendPreference::Graphql),
                    route_target: None,
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let (prefixes, addresses) = client.graphql_vrf_bundle(42, 500).await.expect("bundle");

        // GraphQL's string ids / plain-string status / `_depth` are reshaped into
        // the REST wire models.
        assert_eq!(prefixes.len(), 2);
        assert_eq!(prefixes[0].id, 1);
        assert_eq!(prefixes[0].prefix, "10.50.0.0/16");
        assert_eq!(prefixes[0].depth, Some(0));
        assert_eq!(
            prefixes[0].status.as_ref().map(|c| c.value.as_str()),
            Some("container")
        );
        assert_eq!(prefixes[1].id, 2);
        assert_eq!(prefixes[1].depth, Some(1));
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].id, 9);
        assert_eq!(addresses[0].address, "10.50.1.1/24");
        assert_eq!(addresses[0].dns_name.as_deref(), Some("gw.customer"));

        // The children come back in a SINGLE round-trip, and both lists are
        // scoped by the resolved vrf id.
        let requests = server.received_requests().await.unwrap();
        let bundles: Vec<_> = requests
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains("ip_address_list(filters"))
            .collect();
        assert_eq!(bundles.len(), 1, "VRF children must be one bundled POST");
        let body: Value = serde_json::from_slice(&bundles[0].body).unwrap();
        assert_eq!(body["variables"]["pf"]["vrf_id"], json!({"exact": 42}));
        assert_eq!(body["variables"]["if"]["vrf_id"], json!({"exact": 42}));
    }

    #[test]
    fn gql_rt_vrf_maps_string_id_and_drops_empty_rd() {
        // GraphQL gives a string id and may carry an empty rd; the VrfRef wants a
        // numeric id and a None rd (matching the REST `VrfRef::from_model` path).
        let row: GqlRtVrf =
            serde_json::from_value(json!({"id": "7", "name": "blue", "rd": "65000:7"})).unwrap();
        let r = row.into_ref().expect("vrf ref");
        assert_eq!(r.id, 7);
        assert_eq!(r.name, "blue");
        assert_eq!(r.rd.as_deref(), Some("65000:7"));

        let empty_rd: GqlRtVrf =
            serde_json::from_value(json!({"id": "8", "name": "green", "rd": ""})).unwrap();
        assert!(empty_rd.into_ref().expect("vrf ref").rd.is_none());

        let null_rd: GqlRtVrf =
            serde_json::from_value(json!({"id": "9", "name": "red", "rd": null})).unwrap();
        assert!(null_rd.into_ref().expect("vrf ref").rd.is_none());

        // A non-numeric id drops the row rather than panicking.
        let bad: GqlRtVrf =
            serde_json::from_value(json!({"id": "abc", "name": "x", "rd": null})).unwrap();
        assert!(bad.into_ref().is_none());
    }

    #[test]
    fn gql_vrf_refs_reshapes_preserving_input_order() {
        // Reshape only: string ids → numeric, empty RD → None, a non-numeric id
        // dropped. Order is PRESERVED (the canonical (name, rd) sort is owned by
        // `sort_vrf_refs` downstream, applied to both backends).
        let rows = vec![
            GqlRtVrf {
                id: "3".into(),
                name: "zeta".into(),
                rd: Some("65000:3".into()),
            },
            GqlRtVrf {
                id: "1".into(),
                name: "alpha".into(),
                rd: Some(String::new()),
            },
            GqlRtVrf {
                id: "bad".into(),
                name: "dropped".into(),
                rd: None,
            },
            GqlRtVrf {
                id: "2".into(),
                name: "alpha".into(),
                rd: Some("65000:1".into()),
            },
        ];
        let refs = gql_vrf_refs(rows);
        // "bad" dropped; remaining ids in input order (NOT sorted).
        assert_eq!(refs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 1, 2]);
        // Empty RD normalized to None, matching the REST path.
        assert_eq!(refs[1].rd, None);
    }

    #[tokio::test]
    async fn graphql_route_target_bundle_fetches_both_directions_in_one_query() {
        use crate::config::{ApiConfig, BackendPreference, ProfileConfig};
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Schema probe: route_target_list with a filters arg (no pagination — the
        // bundle selects a single id-filtered row).
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("__schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"__schema": {"queryType": {"fields": [
                    {"name": "route_target_list", "args": [
                        {"name": "filters", "type": {"kind": "INPUT_OBJECT", "name": "RouteTargetFilter"}}
                    ]}
                ]}}}
            })))
            .mount(&server)
            .await;

        // Filter probe: batch A (DeviceFilter…) is empty; batch B (ASNFilter…)
        // carries RouteTargetFilter, which exposes an `id` lookup (4.5 shape) so
        // the bundle can scope to one target. Match the probe on the batch's own
        // marker types (NOT "RouteTargetFilter", which the bundle query's
        // `$rt: RouteTargetFilter` variable would also match).
        let id_field =
            json!({"name": "id", "type": {"kind": "INPUT_OBJECT", "name": "IDFilterLookup"}});
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("DeviceFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("ASNFilter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"routeTarget": {"inputFields": [id_field]}}
            })))
            .mount(&server)
            .await;

        // The bundle itself: one POST carrying the route_target_list selection.
        Mock::given(method("POST"))
            .and(path("/graphql/"))
            .and(body_string_contains("route_target_list(filters"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "route_target_list": [{
                        "importing_vrfs": [
                            {"id": "2", "name": "customer-prod", "rd": "65000:100"},
                            {"id": "5", "name": "customer-dev", "rd": ""}
                        ],
                        "exporting_vrfs": [
                            {"id": "2", "name": "customer-prod", "rd": "65000:100"}
                        ]
                    }]
                }
            })))
            .mount(&server)
            .await;

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: server.uri(),
                api: Some(ApiConfig {
                    search: None,
                    vrf: None,
                    route_target: Some(BackendPreference::Graphql),
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let (importing, exporting) = client
            .graphql_route_target_bundle(42)
            .await
            .expect("bundle");

        // String ids → numeric; empty rd → None; INPUT ORDER preserved (the
        // canonical (name, rd) sort is applied downstream by `sort_vrf_refs`).
        assert_eq!(importing.len(), 2);
        assert_eq!(importing[0].name, "customer-prod");
        assert_eq!(importing[0].id, 2);
        assert_eq!(importing[0].rd.as_deref(), Some("65000:100"));
        assert_eq!(importing[1].name, "customer-dev");
        assert_eq!(importing[1].id, 5);
        assert!(importing[1].rd.is_none());
        assert_eq!(exporting.len(), 1);
        assert_eq!(exporting[0].id, 2);

        // Both directions come back in a SINGLE round-trip, scoped by the id.
        let requests = server.received_requests().await.unwrap();
        let bundles: Vec<_> = requests
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains("route_target_list(filters"))
            .collect();
        assert_eq!(bundles.len(), 1, "RT relations must be one bundled POST");
        let body: Value = serde_json::from_slice(&bundles[0].body).unwrap();
        assert_eq!(body["variables"]["rt"]["id"], json!({"exact": 42}));
    }
}
