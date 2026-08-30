# @balq/node

Node.js bindings for [balq](../../README.md): a local, verified archive of
contract storage built from EIP-7928 Block-Level Access Lists. In-process
(napi-rs), no HTTP hop; reads stay available while `sync()` runs.

```js
const { Archive, Layout, NotAvailableError } = require("@balq/node");

const ar = Archive.open("./balq.redb", { proofWindow: 0 });
ar.watch("0x3582…53ce", 114563);            // from >= head + 1
await ar.sync("http://localhost:8545");     // fetch → verify → apply; returns a SyncReport

const v = ar.storageAt("0x3582…53ce", "0", 114591);
// { value: "0x…0c", provenance: "bal", setAt: 114590, index: 125 }

const layout = Layout.fromFile("./out/Playground.sol/Playground.json"); // forge artifact or bare storageLayout
const loc = layout.locate("balances[0x61Cc…ca80]");
layout.decode(loc, ar.storageAt("0x3582…53ce", loc.slot, 114591).value); // "37585"

try {
  ar.storageAt("0x3582…53ce", "0", 114500);
} catch (e) {
  if (e instanceof NotAvailableError) console.log(e.code); // "BeforeStart" — never null
}
```

Every miss is a `NotAvailableError` with `code` ∈ `NotWatched | BeforeStart |
AfterHead | NotSynced | NotBootstrapped | BootstrapPending | BootstrapLost |
Internal`. Numbers are JS numbers (block heights fit); words, slots and
addresses are 0x-hex strings.

## API

| | |
|---|---|
| `Archive.open(path, {proofWindow?, fullDetail?, allowUnverified?})` | open or create |
| `watch(addr, fromBlock)` / `unwatch(addr)` / `watchlist()` / `head()` | watchlist and head |
| `sync(rpcUrl, bootstrap = true, backupRpc?): Promise<SyncReport>` | one pass to the node's head; `backupRpc` (any archive provider) is asked only when `rpcUrl` lacks a BAL or cannot prove a slot, and is verified the same way |
| `storageAt(addr, slot, block): StorageValue` | one ordered seek |
| `history(addr, slot, from, to): HistoryEntry[]` | changes in `[from, to)` |
| `changedSlots(addr, block): string[]` | from the block index |
| `bootstrapSlot(rpcUrl, addr, slot, backupRpc?): Promise<void>` | prove a never-changed slot at head |
| `Layout.fromFile(path)` / `Layout.fromJson(s)` | solc storageLayout |
| `locate(path)` / `decode(loc, word)` / `describeSlot(slot)` / `fields()` | names ↔ slots |

## Build locally

```
npm ci
npx napi build --platform          # needs Node >= 20.12 for the CLI; the module itself runs on >= 18
node test/smoke.mjs                # against ../../testbed/balq.redb
```

Zero logic lives in this crate — see `src/lib.rs`: argument conversion,
one call into `bal-archive` / `bal-layout`, result conversion.

## Reading by name: `view`

```js
const view = ar.view(proxy, layout).at(114591);   // storage as of the end of block 114591

view.counter            // 12n
view.balances[addr]     // 37585n
view.nested[addr][7]    // 0x… (raw word if the layout cannot decode it)
view.totals.index       // 5000000000000000146n
view.items[3]           // …
view.items.length       // 8n
view.c                  // true
view.lastPoker          // "0x61Cc…"
```

Integers are `bigint`, bools `boolean`, addresses / bytes / undecodable
words `string`. A missing value throws `NotAvailableError` (never
`undefined`); an unknown field throws at access time.

Types: `balq typegen out/Playground.sol/Playground.json > Playground.d.ts`
(or `layout.typescript("PlaygroundView")`), then
`ar.view(proxy, layout).at<PlaygroundView>(block)` — misspelled fields
fail at compile time.
