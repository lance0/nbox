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

    #[test]
    fn resolve_prefers_json_flag_then_output() {
        assert_eq!(Format::resolve(true, Some(Format::Csv)), Format::Json);
        assert_eq!(Format::resolve(false, Some(Format::Csv)), Format::Csv);
        assert_eq!(Format::resolve(false, None), Format::Plain);
    }
}
