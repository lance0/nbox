//! Output rendering for non-TUI commands.
//!
//! Every shell command can emit human-readable plain text (default), JSON
//! (`--json` / `--output json`), or CSV (`--output csv`). JSON/CSV go to stdout
//! so they stay pipe-safe; status/diagnostic messages always go to stderr.

pub mod csv;
pub mod json;
pub mod plain;

use clap::ValueEnum;

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
