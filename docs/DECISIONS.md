# Day 0 — answered 2026-08-29 on Platåberget

Endpoint: `https://rpc.plataberget.ethpandaops.io` (public gateway over reth
v2.5.0, chain id 7091047534, Gloas fork at epoch 1536 / block ≈49152).
Explorer `dora.plataberget.ethpandaops.io`, faucet
`faucet.plataberget.ethpandaops.io`, config in
`github.com/ethpandaops/glamsterdam-devnets` (devnet-8).

| Question | Answer |
|---|---|
| **Q1** BAL on JSON-RPC? | **Yes.** Not on the block object — via `eth_getBlockAccessList(blockNumberOrTagOrHash)` (execution-apis), returned **as JSON**, not RLP. The block object carries `blockAccessListHash`. `debug_getRawBlockAccessList` (RLP) exists in reth but public gateways block `debug_*`. |
| **Q2** BAL for old blocks? | **Yes**, back to block 1 on this node. The EIP lets clients prune after ~3553 epochs (≈2 weeks); the chain is 16 days old, so retention beyond that is not yet observable. |
| Codec vs consensus | `keccak(rlp(BAL decoded from JSON)) == header.blockAccessListHash` on every post-fork block tried (12 blocks across 50k…114k, 188–245 accounts each). Pre-fork blocks have no hash field. |
| `eth_getProof` | Served, but **window = 0**: only at head. reth default `--rpc.eth-proof-window 0`. |
| End to end | `watch` → `sync` → `get`: slot `7744` of the EIP-2935 history contract at block 114228 == hash of block 114227, bit-exact. |

## Consequence of proof window 0

Early bootstrap needs a proof at `C-1` *after* block `C` reveals which slots
changed. With window 0 that is unobtainable by construction — not a race to
win, a value that is gone. On such endpoints:

- slots that change are marked `BootstrapLost { first_seen: C }` at once;
  reads in `[start, C)` for them are unavailable, forever, and say so;
- every post-value is still complete and verified; every never-changed slot
  is still provable at head (lazy bootstrap);
- the affected range is only `[start, first change of that slot)`, i.e. a
  shrinking fraction of history as the archive ages.

On an own reth node, `--rpc.eth-proof-window 128` restores early bootstrap
(and `sync --follow` must keep up within that many blocks). `balq probe`
measures the window; `sync --proof-window N` tells the archive.

## JSON shape (observed, reth 2.5.0)

```
[{ address, storageChanges: [{ key, changes: [{ index, value }] }],
   storageReads: [key], balanceChanges: [{ index, value }],
   nonceChanges: [{ index, value }], codeChanges: [{ index, code }] }]
```

Quantities are minimal hex (`"key":"0x1e29"`). `bal-codec` feature `json`
maps this onto the RLP structures and validates ordering the same way.

## Gateway quirks

The public RPC pools several upstreams at different heights: `eth_blockNumber`
may name a block another upstream reports as not found. `sync` stops the pass
at such a block instead of failing.

---

# Decisions the code enforces (delta from spec v0.2)

Spec v0.2 is the reference (drop it in as `docs/SPEC.md`). The following
deviations came out of the second review and of reading the EIP text on
2026-08-29; the code follows this file where the two differ.

## From the EIP text

- `BlockAccessIndex` is **uint32**, not uint16. Key suffix is 4 bytes.
- All `uint256` fields (slot key, value, balance) are RLP-encoded as minimal
  big-endian bytes, not fixed 32 bytes. `bal-codec` uses `U256`; the archive
  widens to `B256` at its boundary.
- Empty BAL: `rlp([]) = 0xc0`, hash
  `0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347`
  (`EMPTY_BAL_HASH`, tested).
- `storage_reads` also contains slots written with an unchanged value.
  A key may not appear in both lists; the decoder rejects that.

## Provenance in the value (spec open question 4 — answered: yes)

Every stored value is `tag(1) || value(32)`, tag ∈ {Bal, Proof, Imported}.
`meta:bootstrapped` / `meta:imported` sets from v0.2 are gone — they were
redundant with the data and could drift from it.

## Bootstrap records live at `start - 1`

A proven pre-value is written at key `(addr, slot, start-1, u32::MAX)`.
Consequences:

- it never collides with a BAL record (all of which are ≥ `start`);
- it is never inside a valid `history()` range, so history is BAL-only;
- it is never in the block index, so `changed_slots`/`diff` never show a
  phantom change at `start`;
- `storage_at(block ≥ start)` finds it by the same seek as everything else.

## Bootstrap has a deadline

`pending_bootstrap` entries carry `first_seen`. On every sync with a state
source, pending entries older than `bootstrap_window` (default 120 blocks)
become `Lost { first_seen }` — terminal, surfaced as
`NotAvailable::BootstrapLost`. Reads in `[start, first_seen)` for such slots
are unavailable forever; the archive says so instead of guessing.

## Batching is per (address, block)

`eth_getProof(addr, [slots…], block)` takes a slot list. All slots of one
address first seen in block `C` are one call at `C-1`. A burst of thousands
of new slots is a handful of calls per block, not thousands.

## Lazy bootstrap proves at the archive head, not `latest`

`bootstrap_slot` uses `head()` — a block the archive has applied — and the
`state_root` it stored for that block. Using the `latest` tag would race with
changes in `(head, latest]` that the archive has not yet seen.

## `ReadOnlySlot` removed

A slot that was only read has a value; it is obtained by lazy bootstrap like
any never-changed slot. "Read-only" is not a reason for unavailability.

## `watch(from_block)` cannot be in the past

`from_block ≤ head` is `StartInPast`. Backfill (spec §9) is `Archive::backfill`,
an explicit call, not an implicit side effect of `watch`.

## Reorg rollback needs the block index

Keys in the primary index start with `addr || slot`, so "everything above
block N" is **not** a prefix range there. Rollback walks
`blockidx: addr || (N+1).. ` per watched address and deletes the matching
primary records — O(changed records). The v0.2 sentence "rollback = range
delete" is true only because the second index exists.

## Header self-verification: TODO

`keccak(rlp(header)) == blockHash` is not yet checked: the Glamsterdam
header field list is not frozen and rebuilding the RLP wrongly would reject
every block. Until it lands, the trust root is "the node's header for the
canonical block", one notch weaker than spec §1 states. Tracked as the first
item after day 0.

## Built since day 0

- `bal-layout`: solc `storageLayout` → `locate(path)`, `decode`, `describe_slot`
  (flat, packed, struct, mapping, nested mapping, dynamic and fixed arrays).
  Mapping entries cannot be named in reverse (keccak); `diff` shows them `[raw]`.
- `balq get --layout --field`, `balq diff --layout`.
- `balq verify --journal`: compares the archive against rows the sender knows
  to be true (see `testbed/`). Truth without an archive node: our own test
  contract, or contracts whose state is computable from headers (EIP-2935).
- `testbed/`: Playground + EIP-1967 proxy + `poke.mjs` journal writer.
- `bal-node` (napi-rs 3): `Archive` and `Layout` classes, `NotAvailableError.code`,
  async `sync`/`bootstrapSlot` via `spawn_future`. `Archive` is held in an `Arc`
  with no lock — `bal-archive` methods all take `&self` now (redb serialises
  writers itself), so reads work while `sync` runs in the same process.
  Smoke test replays the test-bed journal from JS: 96/96.

## Not built yet

- `subscribe()` / `BlockUpdate` stream
- proxy resolver in `bal-layout` (implementation history → layout per range)
- ERC-7201 / Diamond, dynamic `bytes`/`string`, string-keyed mappings
- `bal-layout-fetch`, `balq serve` (less urgent now: in-process bindings avoid the redb lock)
- Engine API and in-process sources

## Learned from the test bed (2026-08-29)

- **redb is single-process.** While `sync --follow` holds the archive, every
  other `balq` invocation fails with "Database already open". Reads must go
  through the syncing process — this is the concrete reason `balq serve`
  (local HTTP API + the follow loop in one process) comes before any language
  bindings. Until then: stop the follower to query, or use a copy.
- The public gateway returns HTTP 502 (not `null`) for
  `eth_getTransactionReceipt` of a not-yet-mined transaction. Client code that
  polls receipts must treat 5xx-while-pending as "not yet".
- Mapping entries appear as `[raw]` in `diff` by construction (keccak). A
  candidate-key cache (`meta:keycache` in spec v0.2) fed from the watchlist's
  senders / events is the next step for naming them.

# Audit 2026-08-29 (style, docs, review findings)

Workspace lints now: `missing_docs = warn`, `unsafe_code = deny`,
`clippy::unwrap_used` / `expect_used = warn` (tests exempt via
`clippy.toml` + per-crate `#![allow]` in integration tests). Library code
has zero `unwrap`/`expect`; every public item is documented. `rustfmt.toml`,
`.editorconfig`, `LICENSE-MIT` / `LICENSE-APACHE` added.

A multi-agent review (`/code-review high`) confirmed 14 findings; all are
fixed and, where testable, covered:

| Finding | Fix |
|---|---|
| Mid-sync reorg could loop forever on a self-contradicting gateway | same fork twice → `InconsistentSource` error |
| `watch()` during `sync` silently skipped for blocks already snapshotted | watch gate + `in_flight` floor; watchlist snapshot per block under the gate |
| First watched block's parent header never stored → pending bootstrap unretryable | `root_of` fetches and remembers the header |
| `eth_getProof` answer not checked against requested slots; extra slots could plant values | `check_requested` (missing / unexpected slot are errors) |
| Lazy bootstrap could store a post-value after a racing sync (TOCTOU) | head read first; `put_bootstrap` writes only if the slot is unseen or its first change is after the proof block |
| Deep reorg below `start - 1` kept proofs from the orphaned branch | rollback wipes the address entirely; re-proven on demand |
| `bootstrap_slot` before head reached start | `HeadBelowStart` |
| Head check fetched the whole BAL every poll; `NoBal` there blocked sync forever | `BalSource::header()`; `BlockNotFound` at head ends the pass |
| No HTTP timeout — a stalled gateway hung `--follow` silently | 30 s client timeout |
| Blocks applied under `allow_unverified` were stored as `Provenance::Bal` | new `Provenance::Unverified` |
| `retry_pending` scanned every slot on every poll | `pending` index table (migrated on open) |
| Proof `nonce.to::<u64>()` could panic | `try_into` → `Malformed` |
| CLI `--slot` decimal > u64 silently reparsed as hex | decimal is always decimal (`U256`) |
| `Layout::decode` could panic on caller-built locations (Node) | bounds clamped; never panics |
| Fixed arrays of multi-word elements not named by `describe_slot` | shared `describe_elements`, recursive |
| `bootstrap_slot` misused `Corrupt("not watched")`; inverted `history` range returned empty | `NotWatched`, `InvalidRange` |
| `prefix_end` panicked for address `0xff…ff` | unbounded upper bound |

Also added: offline known-answer test of the codec against a real
Platåberget BAL (`bal-codec/tests/consensus.rs`, `--features json`).

## Second pass — edge cases (same day)

| Case | Behaviour now |
|---|---|
| `watch()` for an address already watched | idempotent for the same start; `AlreadyWatched` otherwise (`unwatch` first) |
| `unwatch()` while a block is being applied | taken under the watch gate; `apply_block` re-checks the watchlist inside its transaction — no orphan records |
| two `sync()` passes at once (Node) | second gets `SyncInProgress`; slot released when the pass ends, success or error |
| reopening with a different `full_detail` | `ConfigMismatch` — the flag is stored in `meta` at creation |
| node without a `finalized` tag | fixed horizon `REORG_HORIZON_FALLBACK` (4096) so the header table cannot grow forever |
| gateway answers HTTP 5xx with HTML | `Transport("HTTP 502 … for <method>")` instead of a JSON parse error |
| absurd array index / mapping key in a layout path | slot arithmetic wraps mod 2^256 like the EVM; never panics |
| layout type with `numberOfBytes: 0` | `UnknownType`, not a division by zero |
| Node passes `-1`, `1.5`, `NaN` as a block | rejected with a message; never truncated into a plausible block |
| MSRV | `rust-version = "1.90"` — 1.85 and 1.88 do **not** build (ruint / icu require 1.90); CI checks it |

Repository hygiene: `CHANGELOG.md`, `CONTRIBUTING.md` (release checklist),
`SECURITY.md`, `docs/SPEC.md` (v0.2 with pointers to this file), CI workflow
(fmt, clippy `-D warnings`, rustdoc `-D warnings`, tests on Linux/Windows/macOS,
MSRV check, best-effort Platåberget probe), Cargo metadata for publishing,
versioned path dependencies, repo-local git identity + SSH commit signing.

## Backup source (2026-08-29, late)

`bal_source::Fallback<primary, backup>`. The primary is the node that
decides what the chain is — head, finalized, headers — and that is never
delegated: if the primary is down, sync waits. The backup supplies only
facts that are verified afterwards: a BAL body the primary has pruned
(paired with the primary's header and checked against its hash) and proofs
outside the primary's state window (checked against a stored `state_root`). Trust is unchanged: the archive verifies BALs against the
header and proofs against `state_root` no matter which source answered, so
the backup can be any third-party archive endpoint. It adds reach, not
authority. A lying backup fails verification exactly like a lying primary
(tested).

Practical effect: `sync --rpc <own full node> --backup-rpc <archive provider>`
turns "window 0 → `BootstrapLost`" into "proven via backup", and survives
downtime longer than the primary's window. `bootstrap_window` should then
be set to the *backup's* window (unlimited for an archive endpoint).

## `view` and `typegen` (2026-08-29, late)

The string path (`layout.locate("balances[0x…]")`) stays as the primitive —
the CLI needs it — but it is not the API people should write against.
`archive.view(addr, layout).at(block)` in `balq` is a `Proxy` that
walks the layout (`kindOf`) and reads leaves (`storageAt` + `decodeValue`),
so `view.balances[addr]` reads like the contract. Decisions: integers are
`bigint` (a `number` would silently lose precision), misses throw
`NotAvailableError` (an `undefined` would let `?? 0n` violate promise #3),
unknown fields throw at access. `balq typegen` / `layout.typescript()`
emit a TypeScript interface so field names are checked at compile time.

## Benchmarks (2026-08-29, late)

`balq bench` measures rather than estimates; results in `BENCH.md`. The
one number that contradicts the spec is disk cost: 417–540 B/record vs the
~90 B estimate. Cause: three tables per changed slot plus redb page
overhead. Recorded as a known limitation with the fix order (drop the
bootstrap entry once `Done`, pack the block index, compact) rather than
tuned away before release.

## Backfill and creation: proofs become optional (2026-08-30)

The v0.2 design closed the gap "what was in a slot before its first
recorded change" with `eth_getProof` at `C-1`, inside the node's state
window. On the public Platåberget gateway that window is 0, so every such
slot ended `Lost`, and the fix on offer was a backup archive node. That was
the wrong tool: the gap is a question about *changes*, and changes are what
a full node keeps forever — the BAL is part of the block body.

- **Backfill** walks from the watch start downwards: `header(n)` must equal
  `parent_hash` of the block above (the archive holds the start block's hash,
  or a stored anchor from the previous backfill step), `keccak(rlp(bal))`
  must equal the header's hash. Records land with `Provenance::Bal`; the
  watch start moves down one block per committed transaction, so a killed
  backfill resumes exactly where it stopped. A slot's `first_seen` moves to
  the earliest write found; what is unknown moves below it.
- **Creation.** A verified BAL in which the address receives non-empty code
  that is not an EIP-7702 designator (`0xef0100…`) proves the account had no
  storage before that block (EIP-7610 forbids creation over non-empty
  storage). `CREATED[addr] = block`; the read path returns zero with `bal`
  provenance for any slot with no record, `set_at = creation block`, index
  `u32::MAX`. Pending/lost entries of the address are settled to `Done`.
  Only `verified` BALs may set it; a rollback below the creation block
  removes it.
- **Proofs** stay as an opt-in shortcut (`sync --prove`, `bootstrap_slot`)
  and as the only route to pre-fork state. Defaults no longer touch
  `eth_getProof`. `--backup-rpc` is now "an endpoint that still has old
  blocks", not "an archive".
- **Stops** are typed (`BackfillStop`): target, creation, resolved, budget,
  pre-BAL block (no hash in the header), history unavailable (node dropped
  the block). None of them is an error; all of them are reported.
- **Schema.** New table `created` and meta key `anchor:<addr>`; key layouts
  unchanged, `SCHEMA_VERSION` stays 1 (a v0.1 file opens and gains the
  table on first write).
