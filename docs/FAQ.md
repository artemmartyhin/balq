# FAQ / troubleshooting

### `Database already open. Cannot acquire lock.`

Two processes opened the same `.redb` file — typically `balq index` in one
terminal and `balq get` in another. The file is single-process; run the
indexer with `--serve` and every read command on that file goes through it
automatically (localhost HTTP, sidecar `<archive>.serve`). The Node binding
reads in-process while `sync()` runs.

### `watch from block N is in the past (head H)`

`watch` starts history from *now* (`--from` above the head, or omit it and
pass `--rpc` to use the node's head + 1). History before that is
`backfill`: `balq backfill <addr> --to N`, or without `--to` all the way to
the contract's creation.

### `NOT AVAILABLE (NeverRecorded)` / `<unknown: backfill>`

The slot has no recorded change since the start, and the archive has not
seen the contract's creation, so it does not know the value — and will not
invent zero. `balq backfill <addr>` walks older blocks back to the deploy;
once the creation is seen, every untouched slot is provably zero and this
error disappears for the whole address. (`--prove` / `bootstrapSlot()` is
the shortcut: one `eth_getProof` at the head, if the node serves it.)

### `no record before block N` / `UnknownBefore`

The slot's earliest recorded write is at block N; what was there before is
in some older block's BAL. `balq backfill <addr> --resolve` reads back just
far enough to find it. Values from N on are complete and verified either
way. (`--prove` is an optional `eth_getProof` shortcut for the same value;
backfill does not depend on the node's state window.)

### `balq probe` says `window 0`

The node serves `eth_getProof` only at its head. That limits `--prove`, and
nothing else: sync and backfill never use proofs.

### `the node does not serve block N (history expiry?)`

Backfill reached a block the node has dropped (EIP-4444) or a pruned
backup. Any endpoint that still has old blocks will do as `--backup-rpc`;
it is asked for those blocks only and verified exactly like the primary.

### `block N has no BAL hash (before the BAL fork)`

Backfill reached the Glamsterdam activation. Older storage cannot be read
from blocks; it can only be proven against an archive node
(`sync --prove --backup-rpc <archive>` for slots first seen later), or
stays unknown. Contracts deployed after the fork are not affected.

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
listing all keys does not. With candidates it is named: `diff --keys
0x…,0x…`, and `index` tries every account that appeared in the block (the
sender of a transfer is in its BAL), so `balances[0x61Cc…]` usually shows up
by name in the live output.

### How big will the file get?

Schema v3 stores values minimal (a counter is 1 byte) and one block-index
entry per (address, block). The synthetic bench — worst case, 32-byte random
values, 20 slots per address per block — measures ~450 B per record after
`balq compact`; real contracts (small numbers, few slots per block) land
well below. `balq compact` after a long backfill returns free pages to the
filesystem (4.8 → 2.8 MB on the test-bed archive).
