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
| A JS caller of `@balq/node` | pass any strings/numbers | crash the Node process (a Rust panic aborts it) |
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
- `npm audit` for `@balq/node`: 0 vulnerabilities.
- `cargo machete`: no unused dependencies.
- CI runs with the default `GITHUB_TOKEN` (read-only for PRs); publishing
  requires an `NPM_TOKEN` secret and only on `node-v*` tags.
