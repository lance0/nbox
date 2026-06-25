<!-- What does this change, and why? Link any related issue (e.g. #123). -->

## What

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo clippy --all-targets --no-default-features --locked -- -D warnings`
- [ ] `cargo test --all-features --locked`
- [ ] `cargo test --no-default-features --locked`
- [ ] Generated man pages / shell completions still work if CLI flags changed
- [ ] Live NetBox impact considered if API/query/detail behavior changed
- [ ] `cargo audit` / dependency policy considered if dependencies changed
- [ ] Docs updated, and the kind / tool / keybinding lists kept in sync (if the surface changed)
- [ ] `CHANGELOG.md` `[Unreleased]` updated (user-facing or breaking changes)
- [ ] No secrets or internal-only references included
