# Releasing nbox

How to cut a release and publish. Steps that touch crates.io or push a Git tag
must be run by a maintainer with the right tokens — they're called out below.

Releases are built by the hand-rolled `.github/workflows/release.yml` (no
cargo-dist): pushing a `vX.Y.Z` tag runs audit → build (Linux musl + gnu, macOS
Intel/ARM, Windows) → completions/man → docker (GHCR) → release, attaching a
combined `SHA256SUMS` and auto-generated release notes.

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

## Crate name: already camped

`nbox` **0.1.0 is already published** on crates.io as a name reservation. No
reservation step is needed. crates.io versions are **immutable**, so the first
real release must be **`0.1.1` or higher** — never try to publish `0.1.0`.

## Cut a release

The toolchain floor is **Rust 1.95** (`rust-version` in `Cargo.toml`; the `cache`
feature pulls `libsqlite3-sys`, whose build script needs `cfg_select!`, stable
since 1.95). CI enforces it via the `msrv` job.

1. **Pre-flight (CI enforces all three):**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings   # pedantic gate
   cargo test --all-features                                   # integration tests are #[ignore]; CI runs them separately
   ```
2. **Bump the version.** Set `Cargo.toml` `version = "X.Y.Z"` (≥ 0.1.1), then
   regenerate the lockfile so it isn't dirty at publish time:
   ```bash
   cargo check          # updates Cargo.lock with the new version
   ```
3. **Update the changelog.** Move the `## [Unreleased]` block in
   [CHANGELOG.md](CHANGELOG.md) under a new `## [X.Y.Z] - <date>` heading.
4. **Commit** the bump (stage files explicitly; always include `Cargo.lock` —
   omitting it makes `cargo publish` fail with a dirty-workdir error):
   ```bash
   git add Cargo.toml Cargo.lock CHANGELOG.md
   git commit -m "Bump version to vX.Y.Z"
   ```
5. **Tag and push** (maintainer):
   ```bash
   git tag vX.Y.Z
   git push && git push --tags
   ```

The push fires three workflows; wait for all to go green before publishing:

- **CI** — fmt / clippy / build / test / MSRV.
- **NetBox Integration** — boots netbox-docker 4.2.x and runs the `#[ignore]`
  integration tests (the slow one; several minutes).
- **Release** — the `v*`-tag build matrix below.

## What the release workflow produces

On a `v*` tag, `release.yml` runs five jobs:

1. **audit** — `cargo audit` (RustSec advisory gate).
2. **build** — matrix per target, each archived and uploaded:
   - `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static; also the
     container binaries)
   - `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`, `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc` (`.zip`)
3. **completions** — bash/zsh/fish/powershell/elvish completions + the man page,
   packaged as `nbox-completions.tar.gz`.
4. **docker** — multi-arch (amd64/arm64) image built from the musl binaries via
   `Dockerfile.release`, pushed to GHCR (`ghcr.io/<owner>/nbox`).
5. **release** — gathers every archive into one GitHub Release, generates a
   combined `SHA256SUMS`, and writes auto-generated release notes. Pre-releases
   (`-rc`/`-beta`/`-alpha` tags) are marked as such.

## Homebrew tap

Linux ships **musl** archives (no Windows in Homebrew). After the release exists,
update (or, on the first release, add) `Formula/nbox.rb` in the
`lance0/homebrew-tap` repo, setting `version` and the SHA256 sums from the
release's `SHA256SUMS`. The template lives at
[`packaging/homebrew/nbox.rb`](packaging/homebrew/nbox.rb); it points at the
musl Linux artifacts.

| Archive                                   | Formula location                          |
|-------------------------------------------|-------------------------------------------|
| `nbox-aarch64-apple-darwin.tar.gz`        | `on_macos { on_arm   { sha256 "..." } }`  |
| `nbox-x86_64-apple-darwin.tar.gz`         | `on_macos { on_intel { sha256 "..." } }`  |
| `nbox-aarch64-unknown-linux-musl.tar.gz`  | `on_linux { on_arm   { sha256 "..." } }`  |
| `nbox-x86_64-unknown-linux-musl.tar.gz`   | `on_linux { on_intel { sha256 "..." } }`  |

## Publish to crates.io (last, irreversible)

Once the release and tap are done:

```bash
cargo publish        # version must be >= 0.1.1; cannot be undone
cargo search nbox | grep '^nbox '   # verify
```

## Notes

- Dual-licensed `MIT OR Apache-2.0`; both `LICENSE-MIT` and `LICENSE-APACHE` ship in the crate.
- The published package includes the docs (`DESIGN.md`, `ROADMAP.md`, `docs/`); that's
  fine and tiny (~97 KiB compressed). Add a `Cargo.toml` `exclude = [...]` later only if
  it grows.
- `Cargo.lock` is committed (this is a binary crate) so release builds are reproducible.
