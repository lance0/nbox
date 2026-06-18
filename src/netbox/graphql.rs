//! GraphQL schema capability helpers.
//!
//! NetBox's GraphQL surface has changed across the 4.x line: 4.2 list fields
//! have no pagination argument, 4.3 adds offset pagination, and 4.5 requires
//! lookup objects for ID/enum filters. Rather than branching solely on the
//! version string, nbox probes the schema and shapes filters from the advertised
//! input types.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::netbox::client::NetBoxClient;

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

    /// Shape a filter value for the current NetBox schema.
    ///
    /// Pre-4.5 schemas often expose IDs/enums as scalars or string lists, while
    /// 4.5+ exposes lookup input objects. For lookup inputs, `exact` preserves the
    /// old equality semantics.
    #[must_use]
    pub fn filter_value(&self, filter_type: &str, name: &str, value: Value) -> Option<Value> {
        let field = self.filters.get(filter_type)?.get(name)?;
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
        let schema: SchemaResponse = self.graphql(QUERY_INTROSPECTION, json!({})).await?;
        let first: FilterResponse = self.graphql(FILTER_INTROSPECTION_A, json!({})).await?;
        let second: FilterResponse = self.graphql(FILTER_INTROSPECTION_B, json!({})).await?;
        Ok(GraphqlCapabilities::from_parts(
            schema.schema,
            [first, second],
        ))
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
                let Some(input) = input else {
                    continue;
                };
                let Some(fields) = input.input_fields else {
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
    fn inputs(self) -> [(&'static str, Option<InputType>); 14] {
        [
            ("DeviceFilter", self.device),
            ("SiteFilter", self.site),
            ("IPAddressFilter", self.ip),
            ("PrefixFilter", self.prefix),
            ("VLANFilter", self.vlan),
            ("CircuitFilter", self.circuit),
            ("AggregateFilter", self.aggregate),
            ("ASNFilter", self.asn),
            ("IPRangeFilter", self.ip_range),
            ("TenantFilter", self.tenant),
            ("ContactFilter", self.contact),
            ("ProviderFilter", self.provider),
            ("VirtualMachineFilter", self.virtual_machine),
            ("ClusterFilter", self.cluster),
        ]
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
}
