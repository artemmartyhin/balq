# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Until 1.0, minor versions may break the API
and the on-disk schema (`SCHEMA_VERSION`).

## [0.2.2] — 2026-08-30

### Added
- `Archive::sync_step(source, state, max_blocks)` and `SyncReport::source_head`:
  the forward sync in steps, so `balq index` shows `sync ████ 40/137 blocks · at 117060`
  while catching up instead of a spinner.
- The forward sync fetches blocks 8 at a time like backfill (a reorg discards
  what was prefetched past the fork).
- `JsonRpcSource` retries a call up to 4 times with backoff on transport
  failures (5xx from a gateway, a body cut mid-way); RPC errors are never retried.

## [0.2.1] — 2026-08-30

### Added
- `balq index <addr>… --rpc <url> [--layout C.json]`: the one command — watch,
  catch up, backfill to the deploy, follow. Banner, node line, per-address
  state, progress bar for the backward walk, one line per block with changed
  fields by name (`counter 19 → 20`), empty blocks collapsed. Addresses and
  layout can come from `balq.toml` (`watch = [...]`, `layout = "..."`).
- Backfill fetches 8 blocks concurrently and verifies them in order:
  about 4× faster on a remote node (`bal_archive::FETCH_AHEAD`).
- `sync --follow` and `backfill` use the same renderer and progress bar.

## [0.2.0] — 2026-08-30

### Added
- **Backfill.** `balq backfill <addr>` / `archive.backfill(rpc, addr, opts)` reads
  older blocks' BALs backwards from the watch start — down to the contract's
  creation, to a block, or (`--resolve`) just far enough to know every slot's
  earlier value. Every block is chained by `parent_hash` and its BAL hashed
  against the header; no `eth_getProof`, no archive node, no state window.
- **Creation.** A verified BAL showing the address receiving code marks it
  created; from then on every untouched slot reads as zero with `bal`
  provenance (EIP-7610: no storage before creation). Watch (or backfill to)
  the deploy and nothing is ever `NotBootstrapped` / `Pending` / `Lost`.
- `sync` no longer proves by default; `--prove` (CLI) / `sync(rpc, true)`
  (Node) opts in to the `eth_getProof` shortcut. `--no-bootstrap` is gone.
- `watch` without `--from` starts at the node's head + 1 (`--rpc` or `balq.toml`).
- `status` shows creation per address and how many slots still have an
  unknown earlier value; `get` misses carry the `backfill` command to run;
  `sync --follow` prints one line per pass and one hint, not a log storm.
- `@balq/node` 0.1.2: the published 0.1.1 loader still required the pre-rename
  `balq-<platform>` packages, so `npm i @balq/node` failed with "Cannot find
  native binding" on every platform. `index.js` regenerated for `@balq/node-*`;
  `npm test` and `prepack` now refuse a loader that does not match
  `optionalDependencies`. Package README leads with `view` and `typegen`.
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

### Changed
- Proofs are opt-in; `--no-bootstrap` removed; `ArchiveStats.created`, `NotAvailable` messages point at backfill.
- All crates and `@balq/node` at 0.2.0 (the Node addon changed: `backfill`, `sync(rpc, prove=false)`).

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

## [0.1.0] — 2026-08-29

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
