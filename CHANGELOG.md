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
- `@balq/node`: napi-rs bindings, `NotAvailableError.code`, reads during `sync`,
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
