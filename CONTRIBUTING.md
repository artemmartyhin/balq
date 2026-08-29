# Contributing

## Ground rules

The project is defined by three promises (see README): completeness,
verifiability, definite boundaries. A change that weakens any of them is out
of scope no matter how useful. Concretely:

- never store a value that was not checked against a block header (BAL hash
  or state root) unless it is tagged `Provenance::Imported`/`Unverified`;
- never return a zero or `null` where the answer is "unknown" — extend
  `NotAvailable` instead;
- no protocol-specific code in any crate (`adapters/<protocol>` is a red flag).

## Layout

```
crates/bal-codec     wire format only — the single place a spec change lands
crates/bal-source    node access: traits + JSON-RPC + proof verification
crates/bal-archive   storage, sync, reorgs, bootstrap
crates/bal-layout    solc storageLayout → slots and typed values
crates/bal-cli       `balq` — glue, no logic
crates/bal-node      `@balq/node` — glue, no logic
docs/                spec, decisions, audit notes
testbed/             own contract on Platåberget = ground truth for `verify`
```

Dependency direction is strictly downward; `bal-layout` and `bal-archive`
do not know about each other.

## Before opening a PR

```
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --exclude bal-node
cargo doc --workspace --no-deps --all-features     # RUSTDOCFLAGS=-D warnings
```

For `bal-node`: `cd crates/bal-node && npm ci && npx napi build --platform && node test/smoke.mjs`
(the smoke test needs the test-bed archive; see `testbed/README.md`).

Lints are enforced workspace-wide: every public item is documented,
`unwrap`/`expect` are not used in library code, `unsafe` is denied.

## Changing the on-disk format

Any change to a key or value layout in `bal-archive/src/keys.rs` bumps
`SCHEMA_VERSION`. Older files are refused with `SchemaMismatch`; there is no
in-place migration path before 1.0. Additive tables (like `pending`) are
created on open and may be backfilled once.

## Changing the wire format

Only `bal-codec/src/schema.rs` (and `json.rs`) should need to change. The
consensus known-answer test (`tests/consensus.rs`) must keep passing, or be
replaced with a fixture from the new fork — never deleted.

## Release checklist

1. `CHANGELOG.md` updated, version bumped in `Cargo.toml` (workspace) and
   `crates/bal-node/package.json`.
2. CI green on all platforms.
3. `cargo publish --dry-run -p bal-codec` (then source, archive, layout, in that order).
4. Tag `vX.Y.Z` for crates, `node-vX.Y.Z` to trigger the npm prebuild workflow.
