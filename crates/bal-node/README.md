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

const ar = Archive.open("./balq.redb");
ar.watch(proxy, 114563);                 // from here on (must be above the current head)
await ar.sync("http://localhost:8545");  // forward: fetch → verify keccak(rlp(bal)) against the header → apply
await ar.backfill("http://localhost:8545", proxy);   // backward: older blocks, down to the deploy

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
NeverRecorded · UnknownBefore · Internal`.

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

## Sync and backfill — all from BALs

```js
await ar.sync(rpcUrl);                                            // forward, one pass to the node's head
setInterval(() => ar.sync(rpcUrl).catch(console.error), 4000);   // follow mode; resumes after any downtime

await ar.backfill(rpcUrl, proxy);                                 // backward, to the contract's creation
await ar.backfill(rpcUrl, proxy, { to: 100_000 });                // …or to a block
await ar.backfill(rpcUrl, proxy, { resolveOnly: true });          // …or just enough to know every earlier value
await ar.backfillMany(rpcUrl, [proxy, vault, oracle]);           // a protocol: one walk, every block read once
```

Both read `eth_getBlockAccessList` from an ordinary full node and verify
every block (BAL against the header, headers chained by `parent_hash`).
A full node keeps every block, so backfill has no window: it stops at the
deploy (`stopped: "creation"` — from then on every untouched slot is
provably zero), at your `to`, or when the node no longer serves a block
(`"historyUnavailable"` — pass a `backupRpc` that still has it).

`sync` returns `{ blocksApplied, slotsWritten, reorgedTo, … }`; `backfill`
returns `{ from, to, blocksScanned, recordsWritten, slotsResolved,
unresolved, createdAt, stopped }`. Reads are safe during either; a second
concurrent one is refused with an error. `sync(rpc, true)` additionally
proves newly seen slots' earlier values with `eth_getProof` while the
node's state window allows — an optional shortcut, nothing more.

## Lower level

| | |
|---|---|
| `Archive.open(path, { fullDetail?, allowUnverified?, proofWindow? })` | open or create |
| `watch(addr, fromBlock)` / `unwatch(addr)` / `watchlist()` / `head()` | watchlist and head |
| `storageAt(addr, slot, block): { value, provenance, setAt, index }` | one raw slot, one ordered seek |
| `history(addr, slot, from, to)` | every change in `[from, to)` |
| `changedSlots(addr, block)` | from the block index |
| `bootstrapSlot(rpcUrl, addr, slot, backupRpc?)` | optional: prove a never-changed slot at the head instead of backfilling |
| `layout.locate(path)` / `decode(loc, word)` / `describeSlot(slot)` / `kindOf(path)` | what `view` is built on |

`provenance` is `"bal"` (verified against the header's BAL hash),
`"proof"` (Merkle proof against `state_root`), or, only if you opted in,
`"unverified"` / `"imported"`.

## What to know

- **History starts at the BAL fork.** A contract that lived before
  Glamsterdam keeps its pre-fork storage unknown (`stopped: "preBal"`)
  unless proven against an archive node. Contracts deployed after the fork
  have complete history.
- **Mappings** cannot be enumerated (keccak is one-way): `balances[user]`
  works, "list all holders" does not.
- **Proxies.** Watch the proxy; the layout is the implementation's.

Docs, design notes, benchmarks and the security audit:
[github.com/artemmartyhin/balq](https://github.com/artemmartyhin/balq).
