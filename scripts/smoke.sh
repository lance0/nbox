#!/usr/bin/env bash
# Release smoke: run the full pre-tag gate locally, in one shot, so a tag never
# moves on a tree that would fail CI or the release build. Mirrors the PR gates
# that are practical on one local host — format, both clippy modes (pedantic),
# both test modes, generated artifacts, and security/supply-chain checks. The
# macOS/Windows platform build matrix and live NetBox jobs stay in GitHub Actions.
#
# Usage:  scripts/smoke.sh    (run from anywhere; it cd's to the repo root)
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$1"; }
require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "$1 not installed — run: cargo install $2 --locked" >&2
        exit 1
    fi
}

step "Format check"
cargo fmt --all -- --check

step "Clippy — all features (pedantic gate)"
cargo clippy --all-targets --all-features --locked -- -D warnings

step "Clippy — no default features"
cargo clippy --all-targets --no-default-features --locked -- -D warnings

step "Tests — all features"
cargo test --all-features --locked

step "Tests — no default features"
cargo test --no-default-features --locked

step "Security audit"
require_cmd cargo-audit cargo-audit
cargo audit

step "Supply-chain policy"
require_cmd cargo-deny cargo-deny
cargo deny check advisories bans sources licenses

step "Build smoke — all features + no default features"
cargo build --all-features --locked
cargo build --no-default-features --locked

step "Feature matrix — intermediate combinations"
# Intermediate feature combinations between all-on and all-off. Mirror the
# feature-matrix CI job so the pre-tag gate catches the same combos locally.
for feat in http clipboard updates; do
    echo "--- smoke: --no-default-features --features $feat ---"
    cargo build --no-default-features --features "$feat" --locked || exit 1
done
# http has feature-gated integration tests (tests/mcp_serve_http_tests.rs).
echo "--- smoke: test --no-default-features --features http ---"
cargo test --no-default-features --features http --locked || exit 1

step "Man pages + shell completions generate"
smoke_dir="$(mktemp -d)"
trap 'rm -rf "$smoke_dir"' EXIT
cargo build --quiet --all-features --locked
./target/debug/nbox man "$smoke_dir/man" >/dev/null
for sh in bash zsh fish powershell elvish; do
    ./target/debug/nbox completions "$sh" >/dev/null
done
test -s "$smoke_dir/man/nbox.1"

printf '\n\033[1;32m✓ smoke passed — gate is green; safe to tag.\033[0m\n'
