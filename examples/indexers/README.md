# Indexers on balq instead of an archive node

An event indexer that reads contract state at an event's block —
`balanceOf(holder)` at the Transfer, a pool's reserves at the Swap — needs an
archive node or a paid archive API for that one call. balq answers it from
its verified archive on a full node, so the indexer's RPC can be an ordinary
full node.

Two ways in, both shown here on WETH on Sepolia:

| | how | for |
|---|---|---|
| **RPC URL** | `balq index … --serve --upstream` and point the indexer's RPC at `http://127.0.0.1:7928` | anything that speaks JSON-RPC: Subsquid, Ponder, Envio's RPC source, scripts |
| **viem transport** | `transport: balq({ fallback: http(RPC_URL) })` from [`@balq/viem`](../../packages/viem) | code that builds its own viem client: Ponder (`rpc` takes a transport), Envio Effects, bots |

With `--upstream` balq answers storage reads at a definite block — getters of
`public` variables, `eth_getStorageAt`, `balq_getField` — and relays the rest
unchanged: logs, blocks, receipts, `latest`, views with logic, other
contracts, every miss. A node refusal (429, 5xx) reaches the indexer as the
node sent it, so its own retry logic applies. `X-Balq: local | upstream |
mixed` on each response, and `balq_status` → `served`, say who answered.

## Run it

```sh
# 1. Index WETH, layout from Sourcify, serve as an RPC endpoint.
balq index 0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9 \
  --rpc https://ethereum-sepolia-rpc.publicnode.com \
  --layout auto --history 2000 --serve --upstream

# 2. Pick a range inside the archive's history (`balq status`), then one of:
cd ponder   && npm i && START_BLOCK=… END_BLOCK=… npx ponder start --schema demo
cd subsquid && npm i && FROM=… TO=… node main.js
cd envio    && npm i && npx envio codegen && npx envio dev     # Docker + Postgres
```

Some public gateways refuse `eth_getLogs` without an address filter (Subsquid
asks that way); give `--upstream` an endpoint that allows it — the BALs can
still come from `--rpc`.

## What was measured (2026-10-17, Sepolia, blocks 11936100..=11936300)

- **Ponder 0.18** with `rpc: "http://127.0.0.1:7928"`: 263 Transfers, 33
  holders; every `balanceOf` it stored equals the node's `eth_call` at that
  block (33/33). 383 requests answered from the archive, 320 relayed (logs,
  blocks, and reads of slots whose first write the archive had not reached
  yet — `backfill --resolve` turns those local).
- **Subsquid** (`@subsquid/evm-processor` 1.30) on the same range through the
  relay: the same 33 holders with the same balances and blocks as Ponder.
- **Envio 3.14**: the handler type-checks against `envio codegen` output;
  not run here (`envio dev` needs Docker).

## Limits

- Served locally only for addresses balq indexes and blocks inside their
  history; before the history starts (or before the BAL fork) the read goes
  to the upstream, which then needs that state — for old blocks, an archive.
- A view with logic (`getReserves()` computed, `balanceOf` of a rebasing
  token) is not a storage read; it goes to the upstream at any block. USDC's
  `balanceOf` is one of those — read `balanceAndBlacklistStates[addr]` by
  name with `balq_getField` instead.
- Mappings are answered by key; balq cannot list a mapping's keys.
