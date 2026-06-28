# Continuous Integration

nbox uses a balanced PR gate: deterministic Rust checks run in parallel, live
NetBox checks cover the real API paths, and broader compatibility/security checks
run on schedules before releases.

## Pull request checks

PRs run these checks:

- `fmt`
- `clippy-all-features`
- `clippy-no-default-features`
- `test-all-features`
- `test-no-default-features`
- `msrv-check`
- `generated-artifacts`
- `security-audit`
- `feature-matrix (http)`
- `feature-matrix (clipboard)`
- `feature-matrix (updates)`
- `musl-lean-check`
- `platform-build (x86_64-unknown-linux-musl)`
- `live NetBox 4.2 end-to-end`
- `live NetBox 4.5 GraphQL backend`
- `live NetBox 4.6 compatibility`

PRs build only the musl platform lane; the macOS and Windows platform builds
(`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`) run on
master pushes, with release.yml as the final pre-tag gate. The platform lanes are
build-only on PRs. Linux remains the full test platform so PR feedback stays close
to the current wall time while still catching platform compile failures before
release.

## Scheduled checks

- `NetBox Integration` runs nightly to additionally catch image drift for the
  pinned 4.2/4.5/4.6 fixtures that already gate every PR.
- `Security` runs weekly and executes `cargo audit` plus `cargo deny check
  advisories bans sources licenses` against `deny.toml`.

Scheduled failures should be treated as release blockers even when they do not
block an individual PR.

## Local pre-tag smoke

Run the local gate before tagging:

```bash
scripts/smoke.sh
```

It mirrors the host-local PR gates plus supply-chain checks. GitHub Actions still
owns macOS/Windows platform builds and live NetBox fixtures.
