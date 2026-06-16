//! Custom-field handling for detail views.
//!
//! NetBox returns per-object `custom_fields` as a `{ name: value }` map where
//! unset fields are `null`. We surface the non-empty entries as `cf.<name>`
//! rows (plain) and a `custom_fields` object (JSON).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::output::plain::KeyValues;

/// The non-empty custom fields from a model's `custom_fields` value, ordered by
/// name. Drops `null` and empty-string values.
pub fn fields(custom_fields: &Value) -> BTreeMap<String, Value> {
    custom_fields
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, v)| !is_empty(v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Append `cf.<name>: <value>` rows to a plain key/value block.
pub fn append(kv: &mut KeyValues, fields: &BTreeMap<String, Value>) {
    for (name, value) in fields {
        kv.push(format!("cf.{name}"), display(value));
    }
}

/// Render a custom-field value for plain output.
fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn is_empty(value: &Value) -> bool {
    value.is_null() || matches!(value, Value::String(s) if s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_null_and_empty_keeps_typed_values() {
        let cf = json!({
            "ticket": "INC-42",
            "owner": null,
            "notes": "",
            "monitored": true,
            "rack_units": 4
        });
        let fields = fields(&cf);
        assert_eq!(fields.len(), 3);
        assert_eq!(display(&fields["ticket"]), "INC-42");
        assert_eq!(display(&fields["monitored"]), "true");
        assert_eq!(display(&fields["rack_units"]), "4");
        assert!(!fields.contains_key("owner"));
        assert!(!fields.contains_key("notes"));
    }

    #[test]
    fn non_object_yields_empty() {
        assert!(fields(&json!(null)).is_empty());
    }
}
