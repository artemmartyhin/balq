# @balq/viem

Read your contracts' state from a local [balq](https://github.com/artemmartyhin/balq)
archive instead of an archive RPC — with the code you already have.

```
npm i @balq/viem viem
balq index 0xYourContract --rpc $NODE --layout out/YourContract.sol/YourContract.json --serve
```

```ts
import { createPublicClient, http } from "viem";
import { balq } from "@balq/viem";

const client = createPublicClient({
  chain,
  transport: balq({ fallback: http(process.env.RPC_URL) }),
});

// Exactly as before. Now answered by balq when it can, by the RPC when it cannot.
await client.readContract({ address, abi, functionName: "balanceOf", args: [user], blockNumber: 18_000_000n });
await client.getStorageAt({ address, slot: "0x5", blockNumber: 18_000_000n });

// Private variables have no getter; ask by name.
await client.request({ method: "balq_getField", params: [address, "_reserves[0x…]", "0x112a880"] });
```

## What is served locally

| call | served by balq when |
|---|---|
| `eth_getStorageAt(addr, slot, block)` | `addr` is indexed and `block` is in the archive |
| `eth_call({ to, data }, block)` | `data` is the compiler-generated getter of a `public` variable in `to`'s layout — values, mappings (`balances(addr)`), nested mappings, arrays (`items(i)`), structs (the members as a tuple). Verified against the block header, ABI-encoded like the node would. |
| `balq_getField(addr, "path", block)` | any variable by name, `private` included: `{ value, kind, slot, setAt, provenance }` |
| `eth_blockNumber` | the archive head |

Everything else — `eth_call` on a view with logic (`getReserves()`), other
contracts, logs, transactions, `eth_chainId` — goes to `fallback` untouched.
So does every **miss** (`NotWatched`, `AfterHead`, `BeforeStart`,
`NeverRecorded`, `UnknownBefore`), unless `strict: true`, in which case a
miss throws `BalqRpcError` with `.balqCode`.

`latest` is served from balq only while the archive is at most `headLag`
blocks (default 1) behind the node; otherwise the fallback answers, so a
read-then-send flow never sees stale state.

## Options

```ts
balq({
  url: "http://127.0.0.1:7928",   // balq index --serve
  fallback: http(RPC_URL),        // strongly recommended
  strict: false,                  // true: throw on misses instead of falling back
  addresses: ["0x…"],             // route only these through balq
  headLag: 1,                     // 0 disables the head check for `latest`
})
```

Works with anything built on viem — Ponder, wagmi's server side, your own
indexer or bot. The archive is verified against block headers; the RPC is
never trusted for what balq answers.
