//! CSV output (RFC 4180-ish), derived generically from serialized values.
//!
//! - An array of objects renders as a table (one row per element). Columns are
//!   the requested `cols`, or the union of keys in first-seen order.
//! - A single object renders as two columns: `field,value`.
//! - Scalars/complex values are stringified (objects/arrays as compact JSON).

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

/// Serialize `value` and print it as CSV to stdout.
pub fn print<T: Serialize>(value: &T) -> Result<()> {
    let value = serde_json::to_value(value)?;
    print!("{}", to_csv(&value, None));
    Ok(())
}

/// Render a JSON value as CSV. `cols` (when given) selects/orders columns for
/// the array-of-objects case.
pub fn to_csv(value: &Value, cols: Option<&[String]>) -> String {
    match value {
        Value::Array(items) => array_csv(items, cols),
        Value::Object(_) => object_csv(value),
        other => format!("{}\n", escape(&cell(other))),
    }
}

fn array_csv(items: &[Value], cols: Option<&[String]>) -> String {
    let columns: Vec<String> = match cols {
        Some(c) => c.to_vec(),
        None => infer_columns(items),
    };

    let mut out = String::new();
    out.push_str(&row(columns.iter().map(String::as_str)));
    for item in items {
        let values = columns
            .iter()
            .map(|c| item.get(c).map(cell).unwrap_or_default());
        out.push_str(&row_owned(values));
    }
    out
}

fn object_csv(value: &Value) -> String {
    let mut out = String::new();
    out.push_str(&row(["field", "value"].into_iter()));
    if let Some(map) = value.as_object() {
        for (k, v) in map {
            out.push_str(&row_owned([k.clone(), cell(v)].into_iter()));
        }
    }
    out
}

/// Columns = keys of the objects, in first-seen order across all items.
fn infer_columns(items: &[Value]) -> Vec<String> {
    let mut seen = Vec::new();
    for item in items {
        if let Some(map) = item.as_object() {
            for k in map.keys() {
                if !seen.iter().any(|c| c == k) {
                    seen.push(k.clone());
                }
            }
        }
    }
    seen
}

fn row<'a>(fields: impl Iterator<Item = &'a str>) -> String {
    let escaped: Vec<String> = fields.map(escape).collect();
    format!("{}\n", escaped.join(","))
}

fn row_owned(fields: impl Iterator<Item = String>) -> String {
    let escaped: Vec<String> = fields.map(|f| escape(&f)).collect();
    format!("{}\n", escaped.join(","))
}

/// Stringify a single JSON value for a CSV cell.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// RFC 4180 escaping: quote fields containing comma, quote, CR, or LF.
fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn array_of_objects_becomes_a_table() {
        let v = json!([
            {"kind": "device", "name": "edge01"},
            {"kind": "site", "name": "iad1"}
        ]);
        let csv = to_csv(&v, None);
        assert_eq!(csv, "kind,name\ndevice,edge01\nsite,iad1\n");
    }

    #[test]
    fn cols_select_and_order_columns() {
        let v = json!([{"kind": "device", "id": 1, "name": "edge01"}]);
        let cols = vec!["name".to_string(), "kind".to_string()];
        assert_eq!(to_csv(&v, Some(&cols)), "name,kind\nedge01,device\n");
    }

    #[test]
    fn object_becomes_field_value() {
        let v = json!({"name": "edge01", "status": "active"});
        assert_eq!(
            to_csv(&v, None),
            "field,value\nname,edge01\nstatus,active\n"
        );
    }

    #[test]
    fn escapes_commas_and_quotes() {
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape("plain"), "plain");
    }
}
