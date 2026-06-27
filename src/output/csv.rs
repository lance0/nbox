//! CSV output (RFC 4180-ish), derived generically from serialized values.
//!
//! CSV is tabular-only: an array of objects renders as a table (one row per
//! element). Columns are the requested `cols`, or the union of keys in
//! first-seen order. A single object is rejected — JSON is the canonical shape
//! for structured single objects — see [`to_csv`].
//!
//! Scalars/complex cell values are stringified (nested objects/arrays as
//! compact JSON).

use std::io::Write;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::error::NboxError;

/// The error message shown when `-o csv` is asked for a single object.
const CSV_NOT_TABULAR: &str = "CSV output is only supported for tabular results (arrays). For single objects, use --json or plain text.";

/// Serialize `value` and print it as CSV to stdout.
pub fn print<T: Serialize>(value: &T) -> Result<()> {
    let value = serde_json::to_value(value)?;
    print!("{}", to_csv(&value, None)?);
    Ok(())
}

/// Write CSV directly to a locked stdout, row by row, instead of building one
/// full String. Byte-identical to `print` (and `to_csv`) — `escape` and `cell`
/// are reused unchanged; only the writer target differs (`write!` vs
/// `String::push_str`).
pub fn print_streaming<T: Serialize>(value: &T, cols: Option<&[String]>) -> Result<()> {
    let v = serde_json::to_value(value)?; // still need Value to inspect shape
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match &v {
        Value::Array(items) => {
            // Two-pass: infer columns first (needed before writing header), then stream rows.
            let columns = match cols {
                Some(c) if !c.is_empty() => c.to_vec(),
                _ => infer_columns(items),
            };
            write_header(&mut lock, &columns)?;
            for item in items {
                write_row(&mut lock, item, &columns)?;
            }
        }
        Value::Object(_) => {
            return Err(NboxError::Usage(CSV_NOT_TABULAR.to_string()).into());
        }
        other => {
            writeln!(lock, "{}", escape(&cell(other)))?;
        }
    }
    Ok(())
}

/// Render a JSON value as CSV. `cols` (when given) selects/orders columns for
/// the array-of-objects case.
///
/// CSV is tabular-only: a single [`Value::Object`] is rejected with a usage
/// error ([`NboxError::Usage`], exit code 2) rather than emitting a `field,value`
/// fallback. Arrays and bare scalars are rendered.
pub fn to_csv(value: &Value, cols: Option<&[String]>) -> Result<String> {
    match value {
        Value::Array(items) => Ok(array_csv(items, cols)),
        Value::Object(_) => Err(NboxError::Usage(CSV_NOT_TABULAR.to_string()).into()),
        other => Ok(format!("{}\n", escape(&cell(other)))),
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

/// Write the CSV header line (escaped column names joined by commas, trailing
/// newline) to `w`. Streaming-path sibling of the `row` helper — builds the line
/// as a String then writes it, so the byte output matches `array_csv` exactly.
fn write_header<W: Write>(w: &mut W, columns: &[String]) -> Result<()> {
    let line = row(columns.iter().map(String::as_str));
    w.write_all(line.as_bytes())?;
    Ok(())
}

/// Write one CSV record for `item` (an object) to `w`, selecting `columns` in
/// order; missing keys render as empty cells. Streaming-path sibling of
/// `row_owned` — byte-identical to the `array_csv` row output.
fn write_row<W: Write>(w: &mut W, item: &Value, columns: &[String]) -> Result<()> {
    let values = columns
        .iter()
        .map(|c| item.get(c).map(cell).unwrap_or_default());
    let line = row_owned(values);
    w.write_all(line.as_bytes())?;
    Ok(())
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
        let csv = to_csv(&v, None).unwrap();
        assert_eq!(csv, "kind,name\ndevice,edge01\nsite,iad1\n");
    }

    #[test]
    fn cols_select_and_order_columns() {
        let v = json!([{"kind": "device", "id": 1, "name": "edge01"}]);
        let cols = vec!["name".to_string(), "kind".to_string()];
        assert_eq!(
            to_csv(&v, Some(&cols)).unwrap(),
            "name,kind\nedge01,device\n"
        );
    }

    #[test]
    fn escapes_commas_and_quotes() {
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn escapes_newlines_and_carriage_returns_rfc4180() {
        // RFC 4180: a field containing CR or LF must be quoted (the line break is
        // preserved verbatim inside the quotes, not doubled).
        assert_eq!(escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(escape("a\rb"), "\"a\rb\"");
        assert_eq!(escape("a\r\nb"), "\"a\r\nb\"");
        // Quote-and-newline together: the quote is doubled, the field quoted.
        assert_eq!(escape("a\"\nb"), "\"a\"\"\nb\"");
    }

    #[test]
    fn newline_in_a_cell_value_is_quoted_in_the_table() {
        // A multi-line cell value (e.g. a description) renders as a single quoted
        // field, keeping the embedded newline inside the quotes — one logical row.
        let v = json!([{"name": "edge01", "note": "first\nsecond"}]);
        assert_eq!(
            to_csv(&v, None).unwrap(),
            "name,note\nedge01,\"first\nsecond\"\n"
        );
    }

    #[test]
    fn ragged_objects_union_columns_in_first_seen_order() {
        // Columns are the union of keys across all rows, in first-seen order;
        // a row missing a column emits an empty cell for it.
        let v = json!([
            {"a": 1, "b": 2},
            {"b": 3, "c": 4}
        ]);
        assert_eq!(to_csv(&v, None).unwrap(), "a,b,c\n1,2,\n,3,4\n");
    }

    #[test]
    fn cols_with_an_unknown_column_emits_an_empty_cell() {
        // An explicitly requested column that no row has becomes an empty cell —
        // the header still appears, so the shape is predictable for scripts.
        let v = json!([{"name": "edge01", "kind": "device"}]);
        let cols = vec!["name".to_string(), "missing".to_string()];
        assert_eq!(to_csv(&v, Some(&cols)).unwrap(), "name,missing\nedge01,\n");
    }

    #[test]
    fn empty_array_emits_only_the_inferred_header_row() {
        // No items → no columns to infer → a single (empty) header line.
        assert_eq!(to_csv(&json!([]), None).unwrap(), "\n");
        // With explicit cols, the header is those columns and there are no rows.
        let cols = vec!["kind".to_string(), "name".to_string()];
        assert_eq!(to_csv(&json!([]), Some(&cols)).unwrap(), "kind,name\n");
    }

    #[test]
    fn nested_object_and_array_cells_are_compact_json() {
        // Complex cell values stringify as compact JSON (not pretty-printed); a
        // value containing a comma is then quoted per RFC 4180. Inferred columns
        // follow the serialized key order (serde_json sorts object keys), so the
        // header here is `cf,name,tags`. Pin columns explicitly to keep the focus
        // on the cell encoding rather than ordering.
        let v = json!([{"name": "edge01", "tags": ["a", "b"], "cf": {"x": 1}}]);
        let cols = vec!["name".to_string(), "tags".to_string(), "cf".to_string()];
        assert_eq!(
            to_csv(&v, Some(&cols)).unwrap(),
            "name,tags,cf\nedge01,\"[\"\"a\"\",\"\"b\"\"]\",\"{\"\"x\"\":1}\"\n"
        );
    }

    #[test]
    fn null_and_bool_and_number_cells_stringify() {
        // Scalars render predictably; null is an empty cell.
        let v = json!([{"a": null, "b": true, "n": 7}]);
        assert_eq!(to_csv(&v, None).unwrap(), "a,b,n\n,true,7\n");
    }

    #[test]
    fn single_object_is_rejected_as_non_tabular() {
        // CSV is tabular-only. `-o csv` on a single detail object (e.g. `nbox
        // site`) is rejected with a usage error (exit 2) instead of the old
        // `field,value` fallback; JSON stays the canonical single-object shape.
        let v = json!({
            "name": "iad1",
            "status": "active",
            "custom_fields": {"owner": "neteng"},
            "tags": ["edge", "prod"]
        });
        let err = to_csv(&v, None).unwrap_err();
        assert_eq!(format!("{err:#}"), CSV_NOT_TABULAR);
        // The error carries the stable usage exit code (2).
        assert_eq!(NboxError::exit_code_for(&err), 2);
    }

    #[test]
    fn scalar_value_does_not_panic() {
        // A bare scalar payload (degenerate, but possible) stringifies cleanly.
        assert_eq!(to_csv(&json!("edge01"), None).unwrap(), "edge01\n");
        assert_eq!(to_csv(&json!(42), None).unwrap(), "42\n");
    }
}
