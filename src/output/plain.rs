//! Plain-text output for human-readable detail views.

/// A simple `key: value` renderer for detail output (e.g. `nbox device`).
#[derive(Debug, Default)]
pub struct KeyValues {
    rows: Vec<(String, String)>,
}

impl KeyValues {
    /// Create an empty renderer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a row.
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.rows.push((key.into(), value.into()));
        self
    }

    /// Append a row only when `value` is present.
    pub fn push_opt(
        &mut self,
        key: impl Into<String>,
        value: Option<impl Into<String>>,
    ) -> &mut Self {
        if let Some(v) = value {
            self.push(key, v);
        }
        self
    }

    /// Render all rows as newline-separated `key: value` lines.
    pub fn render(&self) -> String {
        self.rows
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Print the rendered rows to stdout (nothing if empty).
    pub fn print(&self) {
        let rendered = self.render();
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_key_value_lines() {
        let mut kv = KeyValues::new();
        kv.push("name", "edge01")
            .push("status", "active")
            .push_opt("site", Some("iad1"))
            .push_opt("rack", None::<String>);
        assert_eq!(kv.render(), "name: edge01\nstatus: active\nsite: iad1");
    }

    #[test]
    fn empty_renders_to_empty_string() {
        assert_eq!(KeyValues::new().render(), "");
    }
}
