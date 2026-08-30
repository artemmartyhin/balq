# Test bed

A contract we deploy ourselves on Platåberget, so that the **sender knows the
truth**: every `poke()` writes values we can compute off-chain, and the
journal of those values is what `balq verify` compares the archive against —
no archive node required.

`Playground.sol` covers every storage shape `bal-layout` must handle (flat,
packed, struct, mapping, nested mapping, dynamic array) behind a minimal
EIP-1967 proxy (`Proxy1967.sol`), so storage lives at the proxy address and
the layout belongs to the implementation.

Not part of the product. Nothing here is a dependency of any crate.

## One-time setup

```
cd testbed
forge build                         # writes out/, Playground.layout.json is a copy of the storageLayout
npm i                               # ethers
```

`.env` (gitignored) holds `RPC`, `ADDR`, `PK` for a throwaway key. Fund
`ADDR` at https://faucet.plataberget.ethpandaops.io (hCaptcha + browser PoW,
1 ETH minimum per session).

## Run

The whole loop in one go (build, index to the deploy, poke, diff, verify):

```
.\demo.ps1
```

By hand — console 1 keeps running, console 2 writes:

```
node poke.mjs deploy                # once: Playground + proxy -> deploy.json
balq index <proxy> --rpc $RPC --layout Playground.layout.json      # console 1: to the deploy, then follow
node poke.mjs poke 20 15            # console 2: 20 pokes, 15 s apart -> journal.jsonl
node poke.mjs upgrade               # new implementation, journals the 1967 slot
```

Every poke shows up in console 1 as one line: `117022  ▲ 6 record(s)
counter 19 → 20, totals.index …`. Stop it (Ctrl+C; the archive is
single-process) and read:

```
balq verify --journal journal.jsonl
balq diff <proxy> --from A --to B --layout Playground.layout.json
balq get  <proxy> --layout Playground.layout.json --field "balances[$ADDR]" --block B
balq get  <proxy> --layout Playground.layout.json --field totals.index --block B
```

`poke.mjs` mirrors `Playground.poke` in JavaScript (`expected` section).
Change both together.

## What the journal proves

Each row is `(block, address, slot, value)` the sender knows to be true after
its own transaction. `verify` reports `match / mismatch / not_available`.
After `index` reached the deploy nothing is `not_available`: the creation
seen in the BAL settles every pre-value to zero, and every later value is a
verified write.

## Result 2026-08-30

Deployed on Platåberget: implementation `0xf43A4277C415e02c2B2FCe1F4bef8DB890F95959`,
proxy `0x35825972e2ca90851b14576C531F13dA0B5d53ce`, block 114562. A fresh
archive, watched from block 116684 and backfilled to the deploy through the
public gateway (2121 blocks, no proofs):

```
✓ 0x3582…53ce  created at 114562 — history complete (2121 blocks, 113 records)

balq verify --journal journal.jsonl
match:          136
mismatch:       0
not_available:  0

balq get <proxy> --layout Playground.layout.json --field "balances[0x61Cc…]" --block 114562
balances[0x61Cc…] = 0      (slot 0xc0e2…, @ 114562, bal)     # deploy block: zero as a fact from the BAL
balq get <proxy> --layout Playground.layout.json --field "balances[0x61Cc…]" --block 114591
balances[0x61Cc…] = 37585  (slot 0xc0e2…, @ 114590, bal)
```

Gateway quirks met on the way: `eth_getTransactionReceipt` for a pending tx
answers HTTP 502 instead of `null` (`poke.mjs` polls with its own
`waitReceipt`); ethers' `tx.wait()` cannot be used against it. Under load
the gateway serves `eth_getBlockAccessList` in 8–11 s per block instead of
~0.1 s; backfill speed follows the gateway.
