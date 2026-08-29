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

```
node poke.mjs deploy                # Playground + proxy -> deploy.json
balq watch <proxy> --from <block+1> # from deploy.json output
balq sync --rpc $RPC --follow --proof-window 0 &   # keep it running
node poke.mjs poke 20 15            # 20 pokes, 15 s apart -> journal.jsonl
node poke.mjs upgrade               # new implementation, journals the 1967 slot

balq verify --journal journal.jsonl
balq diff <proxy> --from A --to B --layout Playground.layout.json
balq get  <proxy> --layout Playground.layout.json --field "balances[$ADDR]" --block B
balq get  <proxy> --layout Playground.layout.json --field totals.index --block B
```

`poke.mjs` mirrors `Playground.poke` in JavaScript (`expected` section).
Change both together.

## What the journal proves

Each row is `(block, address, slot, value)` the sender knows to be true after
its own transaction. `verify` reports `match / mismatch / not_available`;
`not_available` rows list their reason (`BootstrapLost` is expected for the
pre-value of a slot's first change on endpoints with proof window 0, and
never appears for post-values).

## Result 2026-08-29

Deployed on Platåberget: implementation `0xf43A4277C415e02c2B2FCe1F4bef8DB890F95959`,
proxy `0x35825972e2ca90851b14576C531F13dA0B5d53ce`, block 114562.
12 pokes + 4 `touch()` over blocks 114565..114591, archive followed live
(`sync --follow --poll 3 --proof-window 0`).

```
balq verify --journal journal.jsonl
match:          96
mismatch:       0
not_available:  0
```

```
balq diff <proxy> --from 114570 --to 114574 --layout Playground.layout.json
counter                          3 -> 5
c                                true -> false
totals.index                     3000000000000000611 -> 5000000000000000146
items.length                     3 -> 5
items[3]                         <lost, first change @114572> -> 8976949636…
[raw] 0xc0e28e65…                8558 -> 19358          # balances[me]: mapping, cannot be named in reverse
```

`<lost …>` is the proof-window-0 consequence: the value *before* a slot's
first change is unobtainable on this gateway. Post-values are complete.

Gateway quirks met on the way: `eth_getTransactionReceipt` for a pending tx
answers HTTP 502 instead of `null` (`poke.mjs` polls with its own
`waitReceipt`); ethers' `tx.wait()` cannot be used against it.
