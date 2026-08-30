# @balq/node

Local, verified history of a contract's storage — read by variable name,
at any block, from an ordinary full node. Node.js bindings for
[balq](https://github.com/artemmartyhin/balq) (EIP-7928 Block-Level Access
Lists, Glamsterdam). In-process, no HTTP hop; reads keep working while
`sync()` runs.

```
npm i @balq/node        # prebuilt: win32-x64 · linux-x64 · linux-arm64 · darwin-x64 · darwin-arm64
```

## Read storage like the contract itself

```js
const { Archive, Layout, NotAvailableError } = require("@balq/node");

const ar = Archive.open("./balq.redb", { proofWindow: 0 });
ar.watch(proxy, 114563);                 // start following (must be above the current head)
await ar.sync("http://localhost:8545");  // fetch → verify keccak(rlp(bal)) against the header → apply

const layout = Layout.fromFile("./out/Playground.sol/Playground.json");   // solc storageLayout / forge artifact
const view = ar.view(proxy, layout).at(114591);                          // storage as of block 114591

view.counter            // 12n
view.balances[user]     // 37585n   — mapping by key
view.totals.index       // 5000000000000000146n
view.items[3]           // …
view.items.length       // 8n
view.c                  // true
view.lastPoker          // "0x61Cc…"
view.nested[user][7n]   // nested mappings
```

Integers are `bigint` (a `number` would silently lose precision), bools
`boolean`, addresses/bytes/undecodable words `string`. `private` variables
read the same as `public` ones — this is storage, not getters.

A missing value **throws**, never returns `undefined`:

```js
try {
  ar.view(proxy, layout).at(100).counter;
} catch (e) {
  if (e instanceof NotAvailableError) console.log(e.code);   // "BeforeStart"
}
```

`code` ∈ `NotWatched · BeforeStart · AfterHead · NotSynced · InvalidRange ·
NotBootstrapped · BootstrapPending · BootstrapLost · Internal`.

## Types: `typegen`

```
npx balq typegen out/Playground.sol/Playground.json --name PlaygroundView > Playground.d.ts
```

or from code: `layout.typescript("PlaygroundView")`. Then

```ts
const view = ar.view(proxy, layout).at<PlaygroundView>(114591);
view.balances[user];   // bigint
view.balanses;         // compile error
```

The generated interface mirrors the layout: `bigint` for integers, nested
objects for structs, index signatures for mappings and arrays.

## Sync

```js
await ar.sync(rpcUrl);                       // one pass to the node's head
await ar.sync(rpcUrl, true, backupRpc);      // + an archive endpoint for what the primary cannot serve
setInterval(() => ar.sync(rpcUrl).catch(console.error), 4000);   // follow mode
```

`sync` returns `{ blocksApplied, slotsWritten, bootstrapped, bootstrapPending,
bootstrapLost, reorgedTo, … }`. Reads are safe during a sync; a second
concurrent `sync` is refused with an error. The optional `backupRpc` (any
archive provider) is asked only for BAL bodies and proofs the primary
cannot serve, and is verified the same way — it adds reach, not trust.

## Lower level

| | |
|---|---|
| `Archive.open(path, { proofWindow?, fullDetail?, allowUnverified? })` | open or create |
| `watch(addr, fromBlock)` / `unwatch(addr)` / `watchlist()` / `head()` | watchlist and head |
| `storageAt(addr, slot, block): { value, provenance, setAt, index }` | one raw slot, one ordered seek |
| `history(addr, slot, from, to)` | every change in `[from, to)` |
| `changedSlots(addr, block)` | from the block index |
| `bootstrapSlot(rpcUrl, addr, slot, backupRpc?)` | prove a never-changed slot at the head |
| `layout.locate(path)` / `decode(loc, word)` / `describeSlot(slot)` / `kindOf(path)` | what `view` is built on |

`provenance` is `"bal"` (verified against the header's BAL hash),
`"proof"` (Merkle proof against `state_root`), or, only if you opted in,
`"unverified"` / `"imported"`.

## What to know

- **Forward-only.** History starts at `watch`; nothing before it.
- **Proof window.** Public gateways serve `eth_getProof` only at the head;
  then the value *before* a slot's first change is `BootstrapLost` unless
  you pass a `backupRpc` or run your own node with `--rpc.eth-proof-window`.
  Post-values are never affected.
- **Mappings** cannot be enumerated (keccak is one-way): `balances[user]`
  works, "list all holders" does not.
- **Proxies.** Watch the proxy; the layout is the implementation's.

Docs, design notes, benchmarks and the security audit:
[github.com/artemmartyhin/balq](https://github.com/artemmartyhin/balq).
