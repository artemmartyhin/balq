// Test-bed driver. Deploys Playground behind an EIP-1967 proxy, pokes it
// with known seeds, and writes a journal of (block, address, slot, value)
// that the sender KNOWS to be true. `balq verify --journal` compares the
// archive against this journal — ground truth without an archive node.
//
//   node poke.mjs deploy
//   node poke.mjs poke [count] [intervalSec]
//   node poke.mjs upgrade
//
// Reads testbed/.env (RPC, PK). Writes deploy.json, state.json, journal.jsonl.

import { ethers } from "ethers";
import fs from "node:fs";

const env = Object.fromEntries(
  fs.readFileSync(".env", "utf8").split("\n").filter((l) => l.includes("=")).map((l) => l.split("=").map((s) => s.trim()))
);
// The public gateway pools upstreams and returns intermittent 502s; retry at the transport.
const req = new ethers.FetchRequest(env.RPC);
req.retryFunc = async (_r, resp, attempt) => {
  if (attempt < 8 && resp.statusCode >= 500) { await new Promise((r) => setTimeout(r, 1500)); return true; }
  return false;
};
const provider = new ethers.JsonRpcProvider(req, undefined, { staticNetwork: true, batchMaxCount: 1 });

// The gateway answers HTTP 502 (not `null`) to eth_getTransactionReceipt for a
// transaction it has not seen yet, which makes ethers' tx.wait() throw. Poll
// ourselves and treat transport errors while pending as "not yet".
async function waitReceipt(hash, timeoutMs = 300_000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    try {
      const rc = await provider.getTransactionReceipt(hash);
      if (rc) return rc;
    } catch (_) { /* pending on this upstream */ }
    await new Promise((r) => setTimeout(r, 3000));
  }
  throw new Error(`timeout waiting for ${hash}`);
}
const wallet = new ethers.Wallet(env.PK, provider);
const artifact = (name) => JSON.parse(fs.readFileSync(`out/${name}.sol/${name}.json`, "utf8"));
const pad32 = (x) => ethers.zeroPadValue(ethers.toBeHex(x), 32);
const word = (x) => ethers.zeroPadValue(ethers.toBeHex(BigInt(x)), 32);
const mapSlot = (key32, slot) => ethers.keccak256(ethers.concat([key32, word(slot)]));
const MASK = (bits) => (1n << bits) - 1n;

function loadState() {
  return fs.existsSync("state.json")
    ? JSON.parse(fs.readFileSync("state.json", "utf8"), (k, v) => (typeof v === "string" && /^\d+n$/.test(v) ? BigInt(v.slice(0, -1)) : v))
    : { counter: 0n, balance: 0n, items: [] };
}
function saveState(s) {
  fs.writeFileSync("state.json", JSON.stringify(s, (k, v) => (typeof v === "bigint" ? v.toString() + "n" : v), 1));
}
function journal(lines) {
  fs.appendFileSync("journal.jsonl", lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
}

async function deploy() {
  const P = artifact("Playground"), X = artifact("Proxy1967");
  const impl = await new ethers.ContractFactory(P.abi, P.bytecode.object, wallet).deploy();
  const implRc = await waitReceipt(impl.deploymentTransaction().hash);
  const proxy = await new ethers.ContractFactory(X.abi, X.bytecode.object, wallet).deploy(implRc.contractAddress);
  const rc = await waitReceipt(proxy.deploymentTransaction().hash);
  const out = { playground: implRc.contractAddress, proxy: rc.contractAddress, block: rc.blockNumber, chainId: Number((await provider.getNetwork()).chainId) };
  fs.writeFileSync("deploy.json", JSON.stringify(out, null, 1));
  console.log(out);
  console.log(`\nnext:  balq watch ${out.proxy} --from ${rc.blockNumber + 1}`);
}

async function poke(count = 10, intervalSec = 15) {
  const d = JSON.parse(fs.readFileSync("deploy.json", "utf8"));
  const P = artifact("Playground");
  const c = new ethers.Contract(d.proxy, P.abi, wallet);
  const me = wallet.address;
  const state = loadState();
  for (let i = 0; i < count; i++) {
    const seed = BigInt(ethers.hexlify(ethers.randomBytes(32)));
    const tx = await c.poke(seed);
    const rc = await waitReceipt(tx.hash);
    const blk = await provider.getBlock(rc.blockNumber);

    // --- mirror of Playground.poke ---
    state.counter += 1n;
    const a = seed & MASK(128n), b = (seed >> 128n) & MASK(64n), cc = seed & 1n;
    const lastTime = BigInt(blk.timestamp), index = (state.counter * 10n ** 18n + (seed % 1000n)) & MASK(192n);
    state.balance += seed % 10000n;
    let itemSlotIdx;
    if (state.items.length < 8) { state.items.push(seed); itemSlotIdx = state.items.length - 1; }
    else { itemSlotIdx = Number(state.counter % 8n); state.items[itemSlotIdx] = seed; }

    const dataBase = BigInt(ethers.keccak256(word(5)));
    const balSlot = mapSlot(pad32(me), 3);
    const nestedSlot = mapSlot(word(state.counter), BigInt(mapSlot(pad32(me), 4)));
    const rows = [
      { slot: word(0), value: word(state.counter), field: "counter" },
      { slot: word(1), value: word(a | (b << 128n) | (cc << 192n)), field: "a|b|c" },
      { slot: word(2), value: word(lastTime | (index << 64n)), field: "totals" },
      { slot: balSlot, value: word(state.balance), field: `balances[${me}]` },
      { slot: nestedSlot, value: word(seed), field: `nested[${me}][${state.counter}]` },
      { slot: word(5), value: word(BigInt(state.items.length)), field: "items.length" },
      { slot: word(dataBase + BigInt(itemSlotIdx)), value: word(seed), field: `items[${itemSlotIdx}]` },
      { slot: word(6), value: pad32(me), field: "lastPoker" },
    ].map((r) => ({ block: rc.blockNumber, address: d.proxy, ...r }));
    journal(rows);
    saveState(state);
    console.log(`block ${rc.blockNumber}: poke #${state.counter} seed=${ethers.toBeHex(seed).slice(0, 12)}… (${rows.length} rows)`);
    if (i % 3 === 2) {
      const t = await waitReceipt((await c.touch()).hash); // no-op write → storage_reads
      console.log(`block ${t.blockNumber}: touch() (no-op write)`);
    }
    if (i + 1 < count) await new Promise((r) => setTimeout(r, intervalSec * 1000));
  }
}

async function upgrade() {
  const d = JSON.parse(fs.readFileSync("deploy.json", "utf8"));
  const P = artifact("Playground"), X = artifact("Proxy1967");
  const impl2 = await new ethers.ContractFactory(P.abi, P.bytecode.object, wallet).deploy();
  const impl2Addr = (await waitReceipt(impl2.deploymentTransaction().hash)).contractAddress;
  const proxy = new ethers.Contract(d.proxy, X.abi, wallet);
  const rc = await waitReceipt((await proxy.upgradeTo(impl2Addr)).hash);
  const IMPL_SLOT = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
  journal([{ block: rc.blockNumber, address: d.proxy, slot: IMPL_SLOT, value: pad32(impl2Addr), field: "eip1967.implementation" }]);
  d.playground2 = impl2Addr; d.upgradeBlock = rc.blockNumber;
  fs.writeFileSync("deploy.json", JSON.stringify(d, null, 1));
  console.log(`block ${rc.blockNumber}: upgraded to ${d.playground2}`);
}

const [cmd, ...args] = process.argv.slice(2);
const run = { deploy, poke: () => poke(Number(args[0] ?? 10), Number(args[1] ?? 15)), upgrade }[cmd];
if (!run) { console.error("usage: node poke.mjs deploy | poke [count] [intervalSec] | upgrade"); process.exit(1); }
run().catch((e) => { console.error(e.shortMessage ?? e); process.exit(1); });
