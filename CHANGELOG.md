# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Until 1.0, minor versions may break the API
and the on-disk schema (`SCHEMA_VERSION`).

## [Unreleased]

### Added
- `bal-codec`: EIP-7928 RLP codec with ordering/uniqueness validation,
  `EMPTY_BAL_HASH`, JSON form (`json` feature) matching `eth_getBlockAccessList`;
  known-answer test against a real Platåberget block.
- `bal-source`: `BalSource` / `StateSource` traits, `Fallback` primary+backup source, JSON-RPC implementation with
  30 s timeout, `eth_getProof` verification against `state_root`
  (inclusion and exclusion), request/response slot matching, day-0 probe
  incl. proof-window measurement.
- `bal-archive`: redb store (primary + block index + pending index), provenance
  per value, watch gate against sync races, reorg rollback (incl. below
  `start - 1`), early and lazy bootstrap with post-value protection,
  `full_detail` creation option, typed `NotAvailable`.
- `bal-layout`: solc `storageLayout` → `locate` / `decode` / `describe_slot`
  (flat, packed, struct, mapping, nested mapping, dynamic and fixed arrays).
- `balq` CLI: `probe`, `watch`, `unwatch`, `status`, `sync [--follow]`, `get`,
  `history`, `diff [--layout]`, `verify --journal`.
- `balq`: napi-rs bindings, `NotAvailableError.code`, reads during `sync`,
  `archive.view(addr, layout).at(block)` proxy (Solidity-style reads, `bigint`),
  `layout.typescript()`.
- `balq typegen`: TypeScript interface from a storage layout.
- `--backup-rpc` / `backupRpc`: archive endpoint for BAL bodies and proofs the
  primary cannot serve; chain facts (head, headers) stay with the primary.
- `testbed/`: Playground contract behind an EIP-1967 proxy on Platåberget and a
  journal writer; 96/96 rows verified against the archive.

### Known limitations
- History is forward-only from the watch start; backfill is not implemented.
- On endpoints with `eth_getProof` window 0 (public gateways) and no
  `--backup-rpc`, the value before a slot's first change is unobtainable and
  reported as `BootstrapLost`.
- Header self-hash (`keccak(rlp(header)) == blockHash`) is not checked yet.
- EIP-7928 is in Review; the wire format may change.
- `balq bench`: live (catch-up sync of the most-written addresses, read
  latency vs `eth_getStorageAt`) and synthetic benchmarks; writes
  `results.json` and SVG charts (`docs/bench/`, method in `docs/BENCH.md`).

## [0.1.1] — 2026-08-30

Security release after an independent review (`docs/SECURITY-AUDIT.md`).

### Fixed
- Node process could be killed by untrusted input: a crafted proof node
  panicked inside the trie verifier (now caught and reported as a proof
  error); a layout element type of zero bytes divided by zero in
  `describe_slot`; self-referential mapping/array types overflowed the stack
  in `typescript()` (depth now bounded on every recursion).
- Two races could store a post-value as a pre-value under `proof`
  provenance: `watch()` during the first sync pass, and `unwatch`/deep reorg
  while a proof was in flight. The sync loop now claims its start block
  before its first await, and pre-values are written only if the watch and
  the proof block's hash are unchanged inside the transaction.
- A node answering block N with a header numbered M is refused.
- `find_fork` is capped at the reorg horizon; block hashes are pruned even
  when `finalized` lags; pending retries group in O(n log n) and chunk
  `eth_getProof` calls.
- HTTP response bodies are capped at 64 MiB and read in chunks; redirects
  are not followed; the BAL JSON is no longer cloned before decoding.
- The npm publish job now generates `index.js` / `index.d.ts`; the `node`
  dev-dependency (a binary download on every `npm ci`) is gone.

### Added (unreleased)
- `@balq/node` 0.1.2: package README leads with `view` and `typegen`; the string-path API is documented as the low-level layer. No code change.
- `--json` on every command (one document; `--follow` streams one per pass);
  misses are `{ "error": { "code", "message" } }` with exit code 2.
- `balq.toml` (`rpc`, `backup_rpc`, `proof_window`, `data`); flags win.
- `balq completions <shell>`; `balq status` now reports slot records,
  bootstrap proven/pending/lost, retained headers and file size
  (`Archive::stats`).
- Per-crate READMEs as docs.rs front pages with doctests; `examples/` for
  Rust and Node; `docs/FAQ.md`; `Dockerfile`; `deploy/balq.service`;
  dependabot; issue and PR templates.
- CLI integration tests (`assert_cmd`).
