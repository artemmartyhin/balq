# Security audit — 2026-08-29

Scope: the whole workspace at commit `2e9ea7d` plus the fixes listed below.
Method: threat model first, then every place untrusted bytes enter the
process, then an independent review pass with a different reader, then
fixes with tests. Findings that were fixed are listed with what changed;
findings that were accepted as out of scope are listed with the reason.

## Threat model

| Actor | Can do | Must not be able to |
|---|---|---|
| The primary node | decide the canonical chain (trusted for this by design) | make the archive store a value the chain does not contain, under `bal` or `proof` provenance; crash or hang the process; make it allocate without bound |
| A backup node / archive provider | supply BAL bodies and proofs | anything beyond what the primary can — bodies are checked against the primary's header, proofs against a stored `state_root` |
| Author of a layout file | shape how slots are named and decoded | crash the process, read outside the archive |
| Author of a journal / CLI arguments | drive `verify`, `get`, … | crash the process |
| A JS caller of `balq` | pass any strings/numbers | crash the Node process (a Rust panic aborts it) |
| A crafted archive file | be opened | be silently accepted as consistent |

Not defended against: a primary node that lies about which chain is
canonical (that is the one trust assumption), the local machine, and the
operator's own RPC credentials.

## Surfaces and what protects them

**BAL bytes (RLP or JSON).** Decoded into fixed structures; ordering and
uniqueness validated; `keccak(rlp(bal))` must equal the header field or the
block is refused (`ArchiveError::Verification`). JSON uses
`deny_unknown_fields`; indices must fit `u32`, nonces `u64`. Known-answer
test against a real block pins the wire format.

**Headers.** Parsed into six fields; anything missing is `Malformed`. The
header itself is not yet self-hashed (see "accepted").

**Proofs.** `check_requested` refuses responses with missing or unrequested
slots; the account leaf is verified against `state_root`, each storage leaf
against the account's storage root; zero values are proven by exclusion.
`put_bootstrap` writes only when the slot is unseen or its first change is
after the proof block, so a proof taken at or after a change can never be
stored as a pre-value, whichever source produced it.

**Chain facts.** Head, finalized and headers come from the primary only
(`Fallback` never delegates them). `finalized` is clamped to the head. A
source contradicting itself about a parent link ends the pass with
`InconsistentSource` instead of looping.

**Transport.** 30 s timeout; no redirects; response bodies capped at 64 MiB
and read in chunks against the cap.

**Layout files.** Path resolution is iterative; recursive walks
(`typescript`, `describe_slot`) stop at 32 levels; array arithmetic wraps
mod 2^256; `numberOfBytes = 0` is rejected; `decode` clamps offsets and
sizes and never indexes out of range.

**Archive file.** Every record is length-checked on read; unknown
provenance tags are `Internal` errors; schema and creation-time options are
checked on open; redb detects torn writes.

**Node binding.** All numbers validated (finite, integral, non-negative);
all strings parsed with errors, not panics; the workspace denies
`unwrap`/`expect` in library code so no known panic path remains. Reads
during `sync` are safe (`&self` API, redb MVCC); a second `sync` is refused.

## Findings and fixes

| # | Severity | Finding | Fix |
|---|---|---|---|
| 1 | high | Unbounded response body: a node could answer `eth_getBlockAccessList` with gigabytes and exhaust memory | 64 MiB cap, chunked read (`MAX_BODY_BYTES`) |
| 2 | medium | reqwest followed redirects, so a request could end up at a host the operator never named | `redirect::Policy::none()` |
| 3 | medium | Self-referential type in a layout file overflowed the stack in `typescript()` / `describe_slot()` | depth limit `MAX_NESTING = 32`, test |
| 4 | low | `finalized` above the head from a misbehaving node pruned the head's own hash, breaking later reorg checks | clamped to head |

(Findings from the independent review pass are appended below as they are
confirmed and fixed.)

## Accepted / out of scope

- **Header self-hash** (`keccak(rlp(header)) == blockHash`) is not checked:
  the Glamsterdam header field list is not frozen. Consequence: the node
  can serve a header whose `blockAccessListHash` or `stateRoot` it made up,
  and the archive will verify data against that. This is within the stated
  trust assumption ("the node decides the chain") but wider than the ideal;
  it is the first item after the header format settles.
- **Local file paths** (`--data`, `--layout`, `--journal`) are operator
  input and are used as given.
- **Secrets.** `testbed/.env` holds a throwaway testnet key and is
  git-ignored; nothing in the crates reads environment secrets.

## Supply chain

- `cargo audit`: no vulnerabilities; two unmaintained transitive crates via
  alloy (`derivative`, `paste`).
- `npm audit` for `balq`: 0 vulnerabilities.
- `cargo machete`: no unused dependencies.
- CI runs with the default `GITHUB_TOKEN` (read-only for PRs); publishing
  requires an `NPM_TOKEN` secret and only on `node-v*` tags.

## Independent review pass — findings (all fixed in 0.1.1 unless noted)

| # | Severity | Finding | Fix |
|---|---|---|---|
| 5 | high (Node) | `alloy-trie::verify_proof` has a reachable `unreachable!()` on a crafted in-place extension node; a node could abort the process through a proof | `verify_proof` wrapped in `catch_unwind`; a panic is reported as an invalid proof |
| 6 | high (Node) | `describe_slot` divided by zero for an array element type with `numberOfBytes: 0` | guarded; test |
| 7 | high (Node) | `typescript()` recursed without bound through self-referential mapping / array / fixed-array types (the earlier depth guard only covered structs) | depth incremented on every recursion; test with all four shapes |
| 8 | medium | `watch()` with a start below the first pass's block could be accepted while the pass was fetching, its early blocks skipped, and a later lazy proof stored a post-value as the pre-value | `claim_start()` publishes the in-flight block under the gate before the first await |
| 9 | medium | `unwatch`+`watch` or a deep reorg while a proof was in flight left a stale `Done` pre-value | `put_bootstrap` re-checks the watch start and the proof block's hash inside its transaction; `mark_lost` re-checks the watch |
| 10 | low–medium | Header `number` from the node was never compared with the requested number; mis-routed answers would be filed under the wrong block | checked in the source and again before apply |
| 11 | medium | `from_rpc_json` cloned the whole JSON tree (several × the 64 MiB cap in memory) | deserialised from `&Value` without cloning |
| 12 | low–medium | `find_fork` walked back one RPC call per block without a cap; `HASHES` grew unbounded if `finalized` stalled | walk capped and pruning floored at `REORG_HORIZON_FALLBACK` |
| 13 | low | `retry_pending` grouping was O(P·G); one `eth_getProof` per (address, block) with an unbounded slot list | `BTreeMap` grouping; 256 slots per call |
| 14 | low | `sync()` not cancel-safe: a dropped future left `syncing` set | RAII guard |
| 15 | low | corrupt `first_seen = 0` in the pending table underflowed | `checked_sub` → `Corrupt` |
| 16 | low | `bench` used a predictable shared temp directory and deleted a file in it | `tempfile::tempdir()` |
| 17 | medium (release) | npm publish job never generated `index.js` / `index.d.ts`; the published package would not load | loader uploaded as an artifact from the Linux build and verified before `prepublish` |
| 18 | low–medium | `node` npm dev-dependency ran a binary download on every `npm ci`, including in the publish job | removed; `npm ci --ignore-scripts` in CI |
| 19 | low | workflows had no `permissions:` block | `contents: read` |

Accepted / deferred: `unwatch` and `rollback_to` collect keys into a `Vec` before deleting (memory proportional to the address; correctness unaffected); `bootstrap_slot` can be called in unbounded parallel from JS (bounded only by the node); header self-hash remains the open trust gap noted above.

Reviewed and found sound by the second reader: BAL hash binding and canonicalisation through the RLP encoder; proof slot matching and exclusion proofs; reorg detection and rollback including the below-`start-1` wipe; transport caps; key layout and prefix ranges; `decode` bounds; Node number/string validation; async panics in the binding surfacing as rejected promises.
