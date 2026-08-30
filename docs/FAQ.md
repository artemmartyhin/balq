# FAQ / troubleshooting

### `Database already open. Cannot acquire lock.`

Two processes opened the same `.redb` file — typically `balq sync --follow`
in one terminal and `balq get` in another. The archive is single-process.
Read from the same process (the Node binding does this: `sync()` runs while
`storageAt()` answers), or stop the follower, or query a copy of the file.

### `watch from block N is in the past (head H)`

`watch` starts history from *now*; it is not backfill. Pick a start above
the current head (`eth_blockNumber + 1`). If you need the block you are
past, that is a separate mechanism (`docs/SPEC.md` §9, not built yet).

### `NOT AVAILABLE: slot never changed since watch start and has not been bootstrapped yet`

The slot has no recorded change, so the archive has no value for it — and
will not invent zero. Prove it: `balq get … --rpc <url>` (or
`bootstrapSlot()` in Node) fetches an `eth_getProof` at the head, verifies
it against `state_root`, and stores it. By BAL completeness the value at
the head equals the value at your watch start.

### `lost, first change @N` / `BootstrapLost`

The slot's first change was recorded at block N, but the value *before* N
could not be proven: the node only serves `eth_getProof` inside its state
window (public gateways: the head only, `--proof-window 0`) and the window
passed. Post-values from N on are complete and verified; only reads in
`[start, N)` are unavailable. To avoid it: run `sync --follow` continuously
on a node with `--rpc.eth-proof-window 128` (reth), or pass
`--backup-rpc <archive endpoint>` — the backup's proofs are verified too.

### `balq probe` says `window 0`

Proofs only at the head; see above. Nothing else is affected.

### `reorg deeper than retained block hashes`

The source's chain diverged below the block hashes the archive kept
(`finalized`, or 4096 blocks without a `finalized` tag). The archive will
not guess a fork point. Usually a wrong `--rpc` (different network) or a
devnet reset; check with `balq probe`, then start a new archive file.

### `source is inconsistent around block N`

A pooled gateway served block N with a parent hash that does not match
its own block N-1, twice in a row. The pass stops instead of looping; the
next `--follow` poll retries. Persistent → point at a single node.

### `HASH MISMATCH` in `probe` / `failed BAL verification` in `sync`

`keccak(rlp(bal))` differs from the header's `blockAccessListHash`. Either
the client's BAL encoding changed (EIP-7928 is in Review) or the node is
serving bad data. Nothing is applied. Open an issue with the `probe`
output; the codec's known-answer test pins the format.

### `Package name too similar` / `EOTP` when publishing

Not balq — npm. Scoped names (`@org/name`) avoid the similarity check;
publishing with 2FA on writes needs a passkey/OTP from an interactive
terminal, or an org-level trusted publisher.

### Which address do I watch for a proxy?

The proxy. Storage lives at the proxy's address; the layout comes from the
implementation's compile output. After an upgrade the layout may change —
the implementation history is in the archive (`eip1967.proxy.implementation`
slot), so you know which layout applies to which block range.

### The mapping entry shows as `[raw] 0x…` in `diff`

`keccak` is one-way: a mapping slot cannot be turned back into its key
without a candidate. Reading by a *known* key works (`balances[0x…]`);
listing all keys does not. A candidate cache fed from senders/events is on
the roadmap.

### How big will the file get?

Measured 417–540 bytes per changed slot per block on this build
(`docs/BENCH.md`). A contract with 100 changed slots per block is ~130 GB
a year at that rate; reducing it is the top item on the storage roadmap.
