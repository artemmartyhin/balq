// Quick start for @balq/node: watch a contract, sync, read by name.
//
//   npm i @balq/node
//   node quickstart.mjs https://rpc.plataberget.ethpandaops.io 0x3582… ./out/Playground.sol/Playground.json
//

import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { Archive, Layout, NotAvailableError } = require("@balq/node");

const [rpc = "http://localhost:8545", address, layoutPath] = process.argv.slice(2);
if (!address) {
  console.error("usage: node quickstart.mjs <rpc> <contract address> [storageLayout.json]");
  process.exit(1);
}

const ar = Archive.open("./example.redb");
if (!ar.watchlist().some((w) => w.address.toLowerCase() === address.toLowerCase())) {
  // A new address starts at the node's head (a block that exists).
  const res = await fetch(rpc, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_blockNumber", params: [] }),
  });
  const head = parseInt((await res.json()).result, 16);
  const from = ar.head() ? ar.head().number + 1 : head;
  ar.watch(address, from);
  console.log(`watching ${address} from block ${from}`);
}

const report = await ar.sync(rpc);                       // forward
console.log(`sync: ${report.blocksApplied} block(s), ${report.slotsWritten} record(s)`);
const back = await ar.backfill(rpc, address);            // backward, to the deploy — no proofs
console.log(`backfill: history from ${back.to} (${back.stopped}${back.createdAt ? `, created at ${back.createdAt}` : ""})`);

const head = ar.head();
if (!head) process.exit(0);

// Raw slot 0 at the head.
try {
  const v = ar.storageAt(address, "0", head.number);
  console.log(`slot 0 @ ${head.number}: ${v.value} (${v.provenance}, set at ${v.setAt})`);
} catch (e) {
  if (e instanceof NotAvailableError) console.log(`slot 0: ${e.code} — ${e.message}`);
  else throw e;
}

// By name, if a layout was given: reads like the contract itself.
if (layoutPath) {
  const layout = Layout.fromFile(layoutPath);
  const view = ar.view(address, layout).at(head.number);
  for (const field of layout.fields()) {
    try {
      const kind = layout.kindOf(field);
      if (kind.startsWith("value:")) console.log(`${field} = ${view[field]}`);
      else console.log(`${field}: ${kind} (index it with a key / position)`);
    } catch (e) {
      console.log(`${field}: ${e instanceof NotAvailableError ? e.code : e.message}`);
    }
  }
}
