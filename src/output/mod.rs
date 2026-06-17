//! Output rendering for non-TUI commands.
//!
//! Every shell command can emit human-readable plain text (default), JSON
//! (`--json` / `--output json`), or CSV (`--output csv`). JSON/CSV go to stdout
//! so they stay pipe-safe; status/diagnostic messages always go to stderr.

pub mod csv;
pub mod json;
pub mod plain;

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

/// Render `value` per `format`: JSON (honoring `opts`), CSV, or run `plain` for
/// human text. The single output path shared by every data-producing command.
pub fn emit<T: Serialize>(
    format: Format,
    opts: &json::JsonOptions,
    value: &T,
    plain: impl FnOnce(),
) -> Result<()> {
    match format {
        Format::Json => json::print_with(value, opts)?,
        Format::Csv => csv::print(value)?,
        Format::Plain => plain(),
    }
    Ok(())
}

/// The selected output format for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Format {
    /// Human-readable text.
    #[default]
    Plain,
    /// Pretty-printed JSON.
    Json,
    /// Comma-separated values.
    Csv,
}

impl Format {
    /// Resolve the effective format: the `--json` shortcut wins, else `--output`,
    /// else plain.
    pub fn resolve(json: bool, output: Option<Format>) -> Self {
        if json {
            Format::Json
        } else {
            output.unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NboxError;
    use serde_json::json;

    #[test]
    fn resolve_prefers_json_flag_then_output() {
        assert_eq!(Format::resolve(true, Some(Format::Csv)), Format::Json);
        assert_eq!(Format::resolve(false, Some(Format::Csv)), Format::Csv);
        assert_eq!(Format::resolve(false, None), Format::Plain);
    }

    #[test]
    fn emit_csv_rejects_single_object_with_usage_exit_2() {
        // A single object via `-o csv` is rejected (CSV is tabular-only); the
        // error reaches the process as a usage error (exit code 2).
        let opts = json::JsonOptions::default();
        let value = json!({"name": "iad1", "status": "active"});
        let err = emit(Format::Csv, &opts, &value, || {}).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "CSV output is only supported for tabular results (arrays). For single objects, use --json or plain text."
        );
        assert_eq!(NboxError::exit_code_for(&err), 2);
    }

    #[test]
    fn emit_csv_renders_an_array_as_a_table() {
        // An array/tabular result via `-o csv` still produces CSV (to stdout).
        let opts = json::JsonOptions::default();
        let value = json!([
            {"kind": "device", "name": "edge01"},
            {"kind": "site", "name": "iad1"}
        ]);
        emit(Format::Csv, &opts, &value, || {}).expect("tabular CSV should render");
    }
}
