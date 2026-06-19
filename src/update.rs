//! Update notifications (enabled by the `updates` feature).
//!
//! Checks GitHub releases for a newer version on a background thread and, for
//! interactive CLI runs, prints a notice to stderr. The TUI banner is wired in
//! Phase 3. Ported from ttl, with xfr's `v`-prefix fix.

use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use update_informer::registry::GitHub;

/// How nbox was installed (best guess), so the notice suggests the right upgrade
/// command. Container is detected first (the shipped image is a release binary at
/// a generic path, but `docker pull` is the right upgrade, not the binary URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Container,
    Binary,
}

impl InstallMethod {
    /// Detect the install method: a container marker first, then the executable
    /// path (lowercased once so casing/`Cellar` quirks don't matter), else the
    /// downloaded-binary fallback.
    pub fn detect() -> Self {
        if in_container() {
            return Self::Container;
        }

        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok());

        let Some(path) = exe else {
            return Self::Binary;
        };
        let path = path.to_string_lossy().to_lowercase();

        if path.contains("homebrew") || path.contains("cellar") {
            Self::Homebrew
        } else if path.contains(".cargo/bin") {
            Self::Cargo
        } else {
            Self::Binary
        }
    }

    /// The upgrade command/URL appropriate for this install method.
    pub fn update_command(self) -> &'static str {
        match self {
            Self::Homebrew => "brew upgrade nbox",
            Self::Cargo => "cargo install nbox",
            Self::Container => "docker pull ghcr.io/lance0/nbox",
            Self::Binary => "github.com/lance0/nbox/releases",
        }
    }
}

/// Whether nbox is running inside a container (Docker writes `/.dockerenv`,
/// Podman writes `/run/.containerenv`). Cheap existence checks; both are absent
/// off-Linux and on a bare host, so this falls through to the path heuristics.
fn in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
}

/// Spawn a background thread that checks GitHub for a newer release.
///
/// Uses `interval(ZERO)` to force a live check — this runs at most once per
/// process, so update-informer's cache-based rate limiting is unnecessary.
pub fn spawn_check() -> Receiver<Option<String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(check_for_update());
    });
    rx
}

/// Check GitHub for a newer version, returning `Some(version)` if available.
///
/// Uses update-informer's on-disk cache (`~/.cache/update-informer-rs/`) with a
/// once-a-day interval, so a frequently-run CLI reads a local file on nearly every
/// invocation and only actually hits GitHub at most once per day (the previous
/// `interval(ZERO)` did a network round-trip on every run — every CLI invocation
/// is a fresh process, so "once per process" meant "every time").
pub fn check_for_update() -> Option<String> {
    use update_informer::Check;

    let informer = update_informer::new(GitHub, "lance0/nbox", env!("CARGO_PKG_VERSION"))
        .interval(Duration::from_secs(86_400));

    informer
        .check_version()
        .ok()
        .flatten()
        .map(|v| v.to_string())
}

/// If a result is ready and stderr is an interactive terminal (and not `--json`),
/// print an update notice. Non-blocking beyond a short grace period.
pub fn maybe_print_notice(rx: Receiver<Option<String>>, json: bool) {
    use is_terminal::IsTerminal;

    if json || !std::io::stderr().is_terminal() {
        return;
    }
    if let Ok(Some(version)) = rx.recv_timeout(Duration::from_millis(150)) {
        print_update_notice(&version);
    }
}

/// Print an update notice to stderr as an ASCII box (width-stable across terminals).
pub fn print_update_notice(new_version: &str) {
    // xfr fix: strip a leading `v` so we never render `v -> vv0.2.0`.
    let new_version = new_version.strip_prefix('v').unwrap_or(new_version);
    let current = env!("CARGO_PKG_VERSION");
    let command = InstallMethod::detect().update_command();

    let version_line = format!("Update available: {current} -> {new_version}");
    let command_line = format!("Run: {command}");
    let width = version_line.len().max(command_line.len()) + 4;

    eprintln!();
    eprintln!("\x1b[33m+{}+\x1b[0m", "-".repeat(width));
    eprintln!(
        "\x1b[33m|\x1b[0m  {version_line:<inner$}\x1b[33m|\x1b[0m",
        inner = width - 2
    );
    eprintln!(
        "\x1b[33m|\x1b[0m  {command_line:<inner$}\x1b[33m|\x1b[0m",
        inner = width - 2
    );
    eprintln!("\x1b[33m+{}+\x1b[0m", "-".repeat(width));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_commands_reference_nbox() {
        assert_eq!(
            InstallMethod::Homebrew.update_command(),
            "brew upgrade nbox"
        );
        assert_eq!(InstallMethod::Cargo.update_command(), "cargo install nbox");
        assert_eq!(
            InstallMethod::Container.update_command(),
            "docker pull ghcr.io/lance0/nbox"
        );
        assert!(
            InstallMethod::Binary
                .update_command()
                .contains("github.com/lance0/nbox")
        );
    }
}
