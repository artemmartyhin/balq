# balq — BAL query

Local, verified history of contract storage, built from EIP-7928 Block-Level
Access Lists on an ordinary full node.

> Keep the storage history of your contracts from the moment you install.
> Any slot at any block — locally, on a full node, with completeness checked
> against the block header.

**Forward-only.** History accumulates from the block you start watching.
Pre-install history is a separate, opt-in backfill (not in v1). Continuous
uptime matters: a full node forgets state older than ~128 blocks, and the
pre-values of slots that first change during a longer outage are lost — the
archive records that loss explicitly rather than inventing a value.

## Three promises

1. **Completeness.** If a slot is not in the BAL, it did not change. Not
   "probably": the BAL is consensus-validated via `block_access_list_hash`.
2. **Verifiability.** Every value in the store is checked — against the BAL
   hash in the header, or by Merkle proof against `state_root`. Nothing is
   stored "because the RPC said so"; every record carries its provenance.
3. **Definite boundaries.** A read returns a value or a typed reason it is
   unavailable. Never a silent zero.

The node is trusted for exactly one thing: which block is canonical.

## Status

Works against Platåberget (Glamsterdam public testnet) as of 2026-08-29:

```
balq probe --rpc https://rpc.plataberget.ethpandaops.io
Q1 head         block 114223: VERIFIED — 208 accounts, keccak(rlp(bal)) == header
Q2 old          block  94223: VERIFIED — 216 accounts, keccak(rlp(bal)) == header
eth_getProof    window 0 — proofs only at head.
```

BALs come from `eth_getBlockAccessList`; the codec reproduces the header
hash on real blocks. EIP-7928 is still in Review; the wire format lives in
one crate (`bal-codec`) so a spec change touches one file. See
`docs/DECISIONS.md` for the day-0 findings and what a proof window of 0
means for bootstrap.

## Crates

```
bal-codec    RLP decode, ordering validation, keccak(rlp(bal)), verify   — knows nothing about Solidity
bal-source   BalSource / StateSource traits, JSON-RPC impl, proof verification
bal-archive  redb store, two indexes, sync loop, reorgs, bootstrap
bal-layout   solc storageLayout → slots, typed decode, slot → field name
bal-node     Node.js bindings (napi-rs) — `@balq/node`, zero logic
balq         CLI: probe, watch, sync, get, history, diff, verify, typegen
testbed/     own contract on Platåberget + journal = ground truth for verify
```

## CLI

```
balq probe   --rpc http://localhost:8545                       # day 0: BAL served? proof window?
balq watch   0x3582... --from 114563                           # from must be > current head
balq sync    --rpc http://localhost:8545 --follow [--backup-rpc <archive>] [--proof-window N]
balq get     0x3582... --slot 0 --block 114591 [--rpc ...]     # raw slot
balq get     0x3582... --layout Playground.layout.json --field totals.index --block 114591
balq history 0x3582... --slot 0 --range 114563..114595
balq diff    0x3582... --from 114570 --to 114574 [--layout Playground.layout.json]
balq verify  --journal journal.jsonl                          # archive vs. rows you know are true
balq typegen Playground.layout.json --name PlaygroundView   # TypeScript for @balq/node view
balq status
```

(`0x3582…` is the test-bed proxy on Platåberget, see `testbed/README.md`.)

## Storage layout

```
slots     addr(20) || slot(32) || block(8 BE) || index(4 BE)  ->  provenance(1) || value(32)
blockidx  addr(20) || block(8 BE) || slot(32)                  ->  ()
bootstrap addr(20) || slot(32)                                  ->  state || first_seen
```

A historical read is one ordered seek. The block index makes `changed_slots`,
`diff` and reorg rollback O(changed), not O(all slots).

## Bootstrap

BALs carry post-values only. The value of a slot *before* its first recorded
change comes from `eth_getProof`, verified against the header's `state_root`:

- **early** — the moment a slot first appears in block `C`, prove it at `C-1`
  (still inside the node's state window);
- **lazy** — a slot that never changed since watch start has the same value
  now as then; prove it at the archive head on demand.

Pending proofs expire after the configured window and become `BootstrapLost`.

## Build

```
cargo test --workspace
cargo run -p balq -- --help
```

MSRV 1.90 (checked in CI). Lints: `missing_docs`, `unwrap_used`,
`unsafe_code = deny` (workspace-wide).

## Repository

```
crates/            one crate per layer; dependencies point strictly downward
docs/SPEC.md       the design spec (v0.2)
docs/DECISIONS.md  what the code does differently from the spec, day-0 findings, audit
testbed/           own contract on Platåberget + journal = ground truth for `verify`
.github/           ci (fmt, clippy, docs, tests on 3 OSes, MSRV, probe), npm prebuilds
CHANGELOG.md · CONTRIBUTING.md · SECURITY.md
```

## Status of promises

| | |
|---|---|
| Completeness | by construction of BALs; enforced by `verify()` on every block |
| Verifiability | every stored value carries `Provenance`; only `Imported`/`Unverified` are unchecked, and only opt-in |
| Definite boundaries | `NotAvailable` with a reason on every miss; `NotAvailableError.code` in Node |
| Not yet | header self-hash check; backfill; `subscribe()` stream; ERC-7201 / Diamond layouts |

## Backup node

```
balq sync --rpc http://localhost:8545 --backup-rpc https://<archive-provider> --follow
```

The primary is your full node and decides what the chain is: head, finalized,
headers. That is never delegated — if the primary is down, sync waits. The
backup (an archive endpoint, any provider) is asked only for data the primary
cannot serve: the BAL body of a block it has pruned, or a proof at a block
outside its state window (public gateways serve proofs at the head only). A
backup body is checked against the primary's header hash and a backup proof
against a stored `state_root`, so the backup adds reach, not trust. With a
backup, downtime longer than the primary's window no longer turns first-seen
slots into `BootstrapLost`.
