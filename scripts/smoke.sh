#!/usr/bin/env bash
# Release smoke: run the full pre-tag gate locally, in one shot, so a tag never
# moves on a tree that would fail CI or the release build. Mirrors the CI `check`
# + `audit` jobs — format, both clippy modes (pedantic), both test modes, and
# `cargo audit` — then adds a build smoke and man-page / shell-completion
# generation. The cross-compiled release matrix (musl/darwin/windows) is the
# release workflow's job, not this script's.
#
# Usage:  scripts/smoke.sh    (run from anywhere; it cd's to the repo root)
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }

step "Format check"
cargo fmt --all -- --check

step "Clippy — all features (pedantic gate)"
cargo clippy --all-targets --all-features -- -D warnings

step "Clippy — no default features"
cargo clippy --all-targets --no-default-features -- -D warnings

step "Tests — all features"
cargo test --all-features

step "Tests — no default features"
cargo test --no-default-features

step "Security audit"
if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "cargo-audit not installed — run: cargo install cargo-audit --locked" >&2
    exit 1
fi
cargo audit

step "Build smoke — all features + no default features"
cargo build --all-features
cargo build --no-default-features

step "Man pages + shell completions generate"
smoke_dir="$(mktemp -d)"
trap 'rm -rf "$smoke_dir"' EXIT
cargo run --quiet --all-features -- man "$smoke_dir/man" >/dev/null
for sh in bash zsh fish powershell elvish; do
    cargo run --quiet --all-features -- completions "$sh" >/dev/null
done

printf '\n\033[1;32m✓ smoke passed — gate is green; safe to tag.\033[0m\n'
