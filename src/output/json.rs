//! JSON output for scriptable commands.

use anyhow::Result;
use serde::Serialize;

/// Serialize `value` to a pretty JSON string.
pub fn to_string<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// Print `value` as pretty JSON to stdout.
pub fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", to_string(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_prints_values() {
        let out = to_string(&json!({"name": "edge01", "id": 1})).unwrap();
        assert!(out.contains("\"name\": \"edge01\""));
        // Pretty output is multi-line.
        assert!(out.contains('\n'));
    }
}
