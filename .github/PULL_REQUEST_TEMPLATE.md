## What

## Why

## Checks

- [ ] `cargo fmt --all --check` · `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-features --exclude bal-node`
- [ ] Which of the three promises does this touch (completeness / verifiability / definite boundaries), and how is it still kept?
- [ ] On-disk format unchanged, or `SCHEMA_VERSION` bumped and noted in `CHANGELOG.md`
- [ ] `CHANGELOG.md` updated
