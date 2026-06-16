# Releasing nbox

How to reserve the crate name, cut releases, and publish. Steps that touch
crates.io or GitHub releases must be run by a maintainer with the right tokens —
they're called out below.

## Prerequisites (one-time)

- A crates.io account (sign in at <https://crates.io/> with GitHub).
- An API token from <https://crates.io/settings/tokens>, then:
  ```bash
  cargo login            # paste the token (stored in ~/.cargo/credentials.toml)
  ```
- Verify packaging at any time (no token needed, uploads nothing):
  ```bash
  cargo publish --dry-run
  ```

## 1. Reserve the `nbox` name now (recommended)

`nbox` is currently free on crates.io — reserve it before it can be sniped (we
already lost `nbx` to a collision). crates.io versions are **immutable**, so do
**not** burn `0.1.0` on a placeholder. Publish a pre-release to hold the name and
keep `0.1.0` for the real launch:

```bash
# 1. set a pre-release version
#    Cargo.toml:  version = "0.1.0-alpha.1"
cargo publish --dry-run     # sanity check
cargo publish               # grabs the name (needs `cargo login`)

# 2. put the version back for continued development
#    Cargo.toml:  version = "0.1.0"
```

This publishes a clearly-marked alpha (won't be installed by default), permanently
reserving `nbox`. The real `0.1.0` ships when the launch gate (below) is done.

## 2. Cut the real v0.1.0 release

Ship `0.1.0` once the [ROADMAP](ROADMAP.md) Phase 4 launch gate is met
(release pipeline, install script, Homebrew tap, demo-first README).

1. Ensure green: `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`.
2. Move the `## [Unreleased]` block in [CHANGELOG.md](CHANGELOG.md) under a new `## [0.1.0] - <date>` heading.
3. Confirm `Cargo.toml` `version = "0.1.0"`; commit.
4. Tag and push:
   ```bash
   git tag -a v0.1.0 -m "nbox 0.1.0"
   git push origin v0.1.0
   ```
5. The tag triggers the release workflow (cargo-dist) to build and attach binaries
   (see ROADMAP Phase 4). Then publish the crate:
   ```bash
   cargo publish
   ```

## 3. Binary artifacts (cargo-dist)

Release binaries (macOS Intel/ARM, Linux x86_64/aarch64, Windows) + a Homebrew tap
are produced by [`cargo-dist`](https://github.com/axodotdev/cargo-dist) on tag push
(planned in ROADMAP Phase 4):

```bash
cargo install cargo-dist
cargo dist init            # writes [workspace.metadata.dist] + .github/workflows/release.yml
```

## Notes

- Dual-licensed `MIT OR Apache-2.0`; both `LICENSE-MIT` and `LICENSE-APACHE` ship in the crate.
- The published package includes the docs (`DESIGN.md`, `ROADMAP.md`, `docs/`); that's
  fine and tiny (~97 KiB compressed). Add a `Cargo.toml` `exclude = [...]` later only if
  it grows.
- `Cargo.lock` is committed (this is a binary crate) so release builds are reproducible.
