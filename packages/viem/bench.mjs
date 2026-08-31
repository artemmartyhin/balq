import { createPublicClient, http } from "viem";
import { balq } from "./index.js";
import { readFileSync } from "node:fs";
const RPC = "https://rpc.plataberget.ethpandaops.io";
const proxy = "0x35825972e2ca90851b14576C531F13dA0B5d53ce", user = "0x61Cc0C60bD1cbd6191872fbCA7eeF26f459Fca80";
const abi = JSON.parse(readFileSync("../../testbed/out/Playground.sol/Playground.json", "utf8")).abi;
const call = (c) => c.readContract({ address: proxy, abi, functionName: "balances", args: [user], blockNumber: 114591n });
const time = async (c, n) => { const t = []; for (let i = 0; i < n; i++) { const t0 = performance.now(); await call(c); t.push(performance.now() - t0); } t.sort((a, b) => a - b); return { p50: t[Math.floor(n / 2)].toFixed(1), min: t[0].toFixed(1) }; };
const direct = createPublicClient({ transport: http(RPC) });
const local = createPublicClient({ transport: balq({ fallback: http(RPC), headLag: 0 }) });
let viaRpc; try { viaRpc = await time(direct, 20); } catch (e) { viaRpc = { error: (e.shortMessage || e.message).slice(0, 80) }; }
const viaBalq = await time(local, 20);
console.log(JSON.stringify({ viaRpc, viaBalq, value: String(await call(local)) }));
