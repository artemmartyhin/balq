# balq — BAL query

[![ci](https://github.com/artemmartyhin/balq/actions/workflows/ci.yml/badge.svg)](https://github.com/artemmartyhin/balq/actions/workflows/ci.yml)
[![node prebuilds](https://github.com/artemmartyhin/balq/actions/workflows/bal-node.yml/badge.svg)](https://github.com/artemmartyhin/balq/actions/workflows/bal-node.yml)
[![crates.io](https://img.shields.io/crates/v/balq.svg?label=crates.io%20balq)](https://crates.io/crates/balq)
[![npm](https://img.shields.io/npm/v/@balq/node.svg?label=npm%20%40balq%2Fnode)](https://www.npmjs.com/package/@balq/node)
![MSRV 1.90](https://img.shields.io/badge/MSRV-1.90-informational)
![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

**Local, verified history of any contract's storage — on a full node, no
archive node, no indexer schema.** Built on EIP-7928 Block-Level Access
Lists (Glamsterdam).

```
balq index 0x3582… --rpc http://localhost:8545 --layout Playground.json
  # watches it, walks older blocks back to the deploy, then follows — every block verified
  ✓ 0x3582…53ce  created at 114562 — history complete (2456 blocks, 131 records)
  117022  ▲ 6 record(s)   counter 19 → 20, totals.index …, items[4] 0 → …, [0xc0e2…8cd7] 60428 → 66428

balq get 0x3582… --layout Playground.json --field "balances[0x61Cc…]" --block 114591
→ balances[0x61Cc…] = 37585   (slot 0xc0e2…, set @ 114590, bal)
```

```js
const view = archive.view(proxy, layout).at(114591);   // balq
view.balances[addr]   // 37585n  — reads like the contract, at any block
```

## Why this could not exist before Glamsterdam

Before EIP-7928, "which storage slots changed in this block" lived only
inside the node while it executed the block. To get it you either replayed
blocks with a debug tracer (expensive, client-specific, unverifiable),
trusted the contract's events (only what the author chose to log), or ran an
archive node and asked slot by slot (15 TB, and you must already know which
slots to ask for).

EIP-7928 puts the complete list of changed slots and their post-values
**into the block**, and its hash into the header, under consensus. That
turns "what changed" from a node's opinion into a fact of the block —
complete by rule, verifiable with one keccak. balq is the tool that
accumulates that fact, verifies it, and answers by variable name.

## Three promises

1. **Completeness.** If a slot is not in the BAL, it did not change. Not
   "probably": a block with an incomplete BAL is invalid.
2. **Verifiability.** Every stored value is checked — the BAL against the
   header's hash, every header's hash against its own fields, headers
   against each other by `parent_hash`, and the rare proven value by Merkle
   proof against `state_root`. Every record carries its provenance.
3. **Definite boundaries.** A read returns a value or a typed reason it is
   unavailable. Never a silent zero.

The node is trusted for exactly one thing: which block is canonical.

## Compared with the alternatives

| | Event indexers (Ponder, Envio, Subsquid) | Archive node / provider | Tracing (`debug_traceBlock`) | **balq** |
|---|---|---|---|---|
| What you see | what the author logged | any slot, if you know which | full diff | **every slot, incl. `private`** |
| Completeness | trust the provider's logs | n/a | trust the tracer | **by construction (consensus)** |
| Verification | none | none | none | **keccak vs header, Merkle vs stateRoot** |
| Runs on | anything | 15 TB node or a paid API | archive-grade node | **1–2 TB full node** |
| New question | new schema, reindex | new RPC calls | re-trace | **same file, new query** |
| Historical read | ms (DB) | 50–500 ms (RPC) | seconds | **~2 µs (local seek)** |
| Best at | "what happened" | ad-hoc reads | debugging one tx | **"what state was it in at block N"** |

Indexers are the layer *above* balq, not a competitor: their handlers can
read historical state from balq instead of an archive node.

## Measured (Platåberget, 2026-08-29)

`balq bench --rpc https://rpc.plataberget.ethpandaops.io --blocks 300 --top 20`
— catch-up sync of the 300 latest blocks for the 20 most-written addresses
on the network, release build, laptop, public gateway over HTTPS.
Full tables and method: [`docs/BENCH.md`](docs/BENCH.md).

![historical read latency](docs/bench/read-latency.svg)

![cost per block](docs/bench/per-block.svg)

| | value |
|---|---:|
| historical read, p50 / p99 | **2.2 µs / 3.4 µs** |
| same reads via `eth_getStorageAt`, p50 | 77.8 ms |
| verify one block (keccak of a 215-account BAL) | 0.2 ms |
| apply one block, 20 addresses, ~140 records | 10 ms |
| engine throughput, BALs in memory | 98 blocks/s |
| `eth_getStorageAt` vs archive, 30 control samples | 0 mismatches |
| on-disk cost per record (three indexes, redb) | 417–540 B |

The last line is the honest one: the design estimate was ~90 B/record.
Reducing it is tracked, not hidden.

## How it works

```mermaid
flowchart LR
    N[full node<br/>keeps every block] -- "eth_getBlockAccessList<br/>new blocks (sync) and<br/>old blocks (backfill)" --> C[bal-codec<br/>decode · order check<br/>keccak == header]
    C --> A[bal-archive<br/>redb · reorgs · creation]
    B[second endpoint<br/>optional] -. "old blocks the primary<br/>no longer serves, verified" .-> A
    A --> L[bal-layout<br/>solc storageLayout → names]
    L --> CLI[balq CLI]
    L --> JS["@balq/node<br/>view.balances[addr]"]
```

- **Everything is BAL.** `sync` reads each new block's BAL forward;
  `backfill` reads older blocks' BALs backward from the watch start. Both
  verify the BAL against its header and the headers against each other, so
  a record written from block 100,000 is as verified as one from the head.
  No archive node: a full node keeps every block, and the BAL is part of it.
- **"What was there before?"** is answered by the last earlier write
  (backfill finds it) or by the contract's creation: a verified BAL that
  shows the address receiving code proves there was no storage before it
  (EIP-7610), so every untouched slot is zero — a fact, not a default.
- **Keys** `addr ‖ slot ‖ block ‖ index` — a historical read is one ordered
  seek and a step back. A block index makes `diff` and rollback O(changed).
- **Reorgs.** Parent hashes are checked per block; a fork rolls back through
  the block index.
- **Provenance.** Every value is tagged `bal`, `proof` (optional
  `eth_getProof` shortcut), or (opt-in only) `unverified` / `imported`.

## Install

```
cargo install balq          # CLI, from crates.io
npm i @balq/node            # Node bindings, prebuilt for win32-x64 · linux-x64 · linux-arm64 · darwin-x64 · darwin-arm64
```

## Packages

| Registry | Package | What |
|---|---|---|
| crates.io | [`balq`](https://crates.io/crates/balq) | the CLI |
| crates.io | [`bal-archive`](https://crates.io/crates/bal-archive) | the archive: store, sync, reorgs, bootstrap — use this from Rust |
| crates.io | [`bal-layout`](https://crates.io/crates/bal-layout) | solc `storageLayout` → slots and typed values |
| crates.io | [`bal-source`](https://crates.io/crates/bal-source) | node access: traits, JSON-RPC, proof verification, primary+backup |
| crates.io | [`bal-codec`](https://crates.io/crates/bal-codec) | EIP-7928 wire format only |
| npm | [`@balq/node`](https://www.npmjs.com/package/@balq/node) | Node.js bindings — `Archive`, `Layout`, `view()`; the platform binaries are its `optionalDependencies` |

Rust: `cargo add bal-archive bal-layout` — docs on [docs.rs](https://docs.rs/bal-archive).

## Use

```
balq index    <addr>... --rpc <url> [--layout C.json] [--serve]   # the one command: watch + backfill to the deploy + follow
                                                           #   several contracts: one walk, one file; --layout 0xADDR=C.json per address
                                                           #   --serve: get/diff/history/status work from other terminals meanwhile
balq probe    --rpc <url>                                  # does this node serve BALs, how far back?
balq watch    <addr> [--from N | --rpc <url>]              # the parts of `index`, for scripts:
balq sync     --rpc <url> --follow                         #   forward, verified, resumes after any downtime
balq backfill <addr> --rpc <url> [--to N | --resolve]      #   backward: to the deploy, to block N, or just enough
balq get      <addr> --slot 0 --block N                    # raw slot
balq get      <addr> --layout C.json --field totals.index --block N
balq diff     <addr> --from A --to B [--layout C.json] [--keys 0x…,0x…]   # names where possible; --keys names mapping entries
balq compact                                               # rewrite the file without free pages
balq history <addr> --slot 0 --range A..B
balq verify  --journal rows.jsonl                          # archive vs. rows you know are true
balq typegen C.json --name CView > C.d.ts                  # TypeScript for the Node view
balq bench   [--rpc <url>] [--out docs/bench]              # the numbers above
balq completions bash > /etc/bash_completion.d/balq      # zsh, fish, powershell too
```

Every command takes `--json` for scripts (a miss is `{"error":{"code":"BeforeStart",…}}`
with exit code 2, never a zero). `balq.toml` holds the defaults: `rpc`,
`backup_rpc`, `data`, and for `index` the `watch = ["0x…", …]` list, the default
`layout` and a `[layouts]` table with one layout per address (a protocol of
several contracts) — then it is just `balq index`. Run it as a service with
`deploy/balq.service` or the `Dockerfile`; common errors are explained in
[`docs/FAQ.md`](docs/FAQ.md).

The layout is the `storageLayout` from your compiler (`forge inspect C
storageLayout`, or a forge/hardhat artifact) — not the ABI.

### Node

```js
const { Archive, Layout, NotAvailableError } = require("@balq/node");
const ar = Archive.open("./balq.redb");
ar.watch(proxy, 114563);
await ar.sync(rpcUrl);                               // forward; reads keep working meanwhile
await ar.backfill(rpcUrl, proxy);                    // backward, to the deploy

const layout = Layout.fromFile("out/Playground.sol/Playground.json");
const v = ar.view(proxy, layout).at(114591);
v.counter; v.balances[addr]; v.totals.index; v.items[3]; v.items.length;   // bigint / boolean / string
try { ar.view(proxy, layout).at(1).counter } catch (e) { e.code }          // "BeforeStart", never undefined
```

## What a read can say instead of a value

| code | meaning |
|---|---|
| `BeforeStart` | before the history starts — `backfill --to N` extends it |
| `AfterHead` | sync has not reached that block yet |
| `NeverRecorded` | the slot never changed since the start and the creation was not seen — `backfill` to the deploy |
| `UnknownBefore` | the slot's earliest recorded write is at N, nothing known before it — `backfill --resolve` |

None of these happen for a contract watched from (or backfilled to) its
deploy: creation seen means every value is known.

## Limits, stated

- **History starts at the BAL fork.** Blocks before Glamsterdam carry no
  BAL; a contract that already lived then keeps its pre-fork storage
  unknown unless proven against an archive node (`sync --prove`, or
  `--backup-rpc`). Contracts deployed after the fork have complete history.
- **The node must still serve old blocks.** Backfill reads them; a node with
  history expiry (EIP-4444) answers `HistoryUnavailable` — pass
  `--backup-rpc` with any endpoint that still has them. It is asked for
  blocks only, and verified like the primary.
- **Mappings** cannot be enumerated (keccak is one-way); reading a known key
  works, "list all holders" does not. Naming works from candidates: `index`
  tries every account of the block, `diff --keys` whatever you pass.
- **Layouts** come from solc; ERC-7201 / Diamond namespaces are mounted through
  a manifest (see `bal-layout`), dynamic `bytes`/`string` are read across
  their data slots.
- **Not yet:** `subscribe()` stream, a query language over history.
- **EIP-7928 is in Review.** The wire format lives in one crate and is pinned
  by a known-answer test against a real block; a spec change touches one file
  and fails that test first.

## Repository

```
crates/            bal-codec · bal-source · bal-archive · bal-layout · bal-cli · bal-node
docs/              SPEC.md · DECISIONS.md · BENCH.md · SECURITY-AUDIT.md · bench/
testbed/           own contract on Platåberget + journal = ground truth for `verify` (96/96)
.github/           ci: fmt · clippy · rustdoc · tests on 3 OSes · MSRV · live probe; npm prebuilds
```

MSRV 1.90. Workspace lints: `missing_docs`, `unwrap_used`, `unsafe_code = deny`.
`CHANGELOG.md` · `CONTRIBUTING.md` · `SECURITY.md` · MIT OR Apache-2.0.
