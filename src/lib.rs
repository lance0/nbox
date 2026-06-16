//! nbx — terminal UI and CLI for NetBox.
//!
//! This is the library crate root. Modules (`cli`, `config`, `netbox`, `domain`,
//! `tui`, `output`, …) are introduced as the implementation progresses; see
//! `DESIGN.md` and `ROADMAP.md` for the intended structure and phasing.

/// The crate version, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
