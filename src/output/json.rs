//! JSON output for scriptable / agent consumers.

use std::io::Write;

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

/// Render `value` as a JSON string, applying field selection, envelope, and raw
/// options. The single shaping path behind `print_with` (and directly testable).
pub fn render_with<T: Serialize>(value: &T, opts: &JsonOptions) -> Result<String> {
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
    Ok(rendered)
}

/// Print `value` as JSON, applying field selection, envelope, and raw options.
pub fn print_with<T: Serialize>(value: &T, opts: &JsonOptions) -> Result<()> {
    println!("{}", render_with(value, opts)?);
    Ok(())
}

/// Write a value directly to a locked stdout via `serde_json::to_writer_pretty`,
/// skipping the intermediate `Value` + `String` materialization. Byte-identical
/// to `render_with(value, &JsonOptions::default())` followed by `println!` —
/// `to_writer_pretty` and `to_string_pretty` share the same formatter. Only
/// called when no shaping is needed (`opts.fields.is_none() && !opts.envelope`);
/// `opts.raw` selects the compact writer.
pub fn print_streaming<T: Serialize>(value: &T, opts: &JsonOptions) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if opts.raw {
        serde_json::to_writer(&mut lock, value)?;
    } else {
        serde_json::to_writer_pretty(&mut lock, value)?;
    }
    writeln!(lock)?;
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

    #[test]
    fn render_with_default_is_pretty_unchanged() {
        let v = json!({"name": "edge01", "id": 1});
        let out = render_with(&v, &JsonOptions::default()).unwrap();
        assert!(out.contains('\n'), "pretty output has newlines");
        // No shaping applied: both keys survive.
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn render_with_fields_keeps_only_requested_top_level_keys() {
        let v = json!({"name": "edge01", "id": 1, "site": "iad1"});
        let opts = JsonOptions {
            fields: Some(vec!["name".into(), "site".into()]),
            ..Default::default()
        };
        let back: Value = serde_json::from_str(&render_with(&v, &opts).unwrap()).unwrap();
        assert_eq!(back, json!({"name": "edge01", "site": "iad1"}));
    }

    #[test]
    fn render_with_raw_is_single_line_compact() {
        let v = json!({"name": "edge01", "id": 1});
        let opts = JsonOptions {
            raw: true,
            ..Default::default()
        };
        let out = render_with(&v, &opts).unwrap();
        assert!(!out.contains('\n'), "raw output is single-line: {out:?}");
        assert!(!out.contains(": "), "raw output is compact: {out:?}");
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn render_with_envelope_wraps_payload() {
        let v = json!({"name": "edge01"});
        let opts = JsonOptions {
            envelope: true,
            ..Default::default()
        };
        let back: Value = serde_json::from_str(&render_with(&v, &opts).unwrap()).unwrap();
        assert_eq!(back["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(back["data"], v);
    }

    #[test]
    fn render_with_composes_fields_envelope_and_raw() {
        // --fields name --envelope --raw together: field-select happens *before*
        // the envelope wrap (so `data` is the trimmed object), and the whole
        // thing is emitted compact on one line.
        let v = json!({"name": "edge01", "id": 1, "site": "iad1"});
        let opts = JsonOptions {
            fields: Some(vec!["name".into()]),
            raw: true,
            envelope: true,
        };
        let out = render_with(&v, &opts).unwrap();
        assert!(!out.contains('\n'), "composed raw output is single-line");
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(back["data"], json!({"name": "edge01"}));
    }
}
