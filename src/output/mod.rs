//! Output rendering for non-TUI commands.
//!
//! Every shell command can emit either human-readable plain text (default) or
//! machine-readable JSON (`--json`). JSON goes to stdout so it stays pipe-safe;
//! status/diagnostic messages always go to stderr (see [`crate::run`]).

pub mod json;
pub mod plain;

/// The selected output format for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Human-readable text.
    #[default]
    Plain,
    /// Pretty-printed JSON.
    Json,
}

impl Format {
    /// Derive the format from the global `--json` flag.
    pub fn from_json_flag(json: bool) -> Self {
        if json { Format::Json } else { Format::Plain }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_from_flag() {
        assert_eq!(Format::from_json_flag(true), Format::Json);
        assert_eq!(Format::from_json_flag(false), Format::Plain);
        assert_eq!(Format::default(), Format::Plain);
    }
}
