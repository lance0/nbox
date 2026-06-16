//! JSON output for scriptable / agent consumers.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

/// Version of the `--envelope` wrapper schema.
pub const SCHEMA_VERSION: u32 = 1;

/// JSON shaping options (from `--fields` / `--raw` / `--envelope`).
#[derive(Debug, Clone, Default)]
pub struct JsonOptions {
    /// Keep only these top-level object keys (applied per element for arrays).
    pub fields: Option<Vec<String>>,
    /// Compact (non-pretty) output.
    pub raw: bool,
    /// Wrap output as `{ "schema_version": N, "data": <payload> }`.
    pub envelope: bool,
}

/// Serialize `value` to a pretty JSON string (no shaping).
pub fn to_string<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// Print `value` as pretty JSON to stdout (no shaping).
pub fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", to_string(value)?);
    Ok(())
}

/// Print `value` as JSON, applying field selection, envelope, and raw options.
pub fn print_with<T: Serialize>(value: &T, opts: &JsonOptions) -> Result<()> {
    let mut v = serde_json::to_value(value)?;
    if let Some(fields) = &opts.fields {
        v = select_fields(v, fields);
    }
    if opts.envelope {
        v = serde_json::json!({ "schema_version": SCHEMA_VERSION, "data": v });
    }
    let rendered = if opts.raw {
        serde_json::to_string(&v)?
    } else {
        serde_json::to_string_pretty(&v)?
    };
    println!("{rendered}");
    Ok(())
}

/// Keep only `fields` on objects (recursing into array elements).
fn select_fields(value: Value, fields: &[String]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| fields.iter().any(|f| f == k))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|i| select_fields(i, fields))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_prints_values() {
        let out = to_string(&json!({"name": "edge01", "id": 1})).unwrap();
        assert!(out.contains("\"name\": \"edge01\""));
        assert!(out.contains('\n'));
    }

    #[test]
    fn select_fields_filters_objects_and_arrays() {
        let v = json!([{"id": 1, "name": "a", "x": 9}, {"id": 2, "name": "b", "x": 8}]);
        let got = select_fields(v, &["id".into(), "name".into()]);
        assert_eq!(got, json!([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]));
    }

    #[test]
    fn envelope_wraps_with_schema_version() {
        let v = json!({"name": "edge01"});
        let wrapped = serde_json::json!({ "schema_version": SCHEMA_VERSION, "data": v });
        assert_eq!(wrapped["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(wrapped["data"]["name"], json!("edge01"));
    }
}
