// Integration test: a real viem client over the balq transport against a
// running `balq index --serve` (Playground on Platåberget) with the public
// gateway as fallback.
//
//   BALQ_URL=http://127.0.0.1:7928 RPC_URL=https://rpc.plataberget.ethpandaops.io node test.mjs
import { createPublicClient, http } from "viem";
import { balq, BalqRpcError } from "./index.js";
import { readFileSync } from "node:fs";

const RPC = process.env.RPC_URL || "https://rpc.plataberget.ethpandaops.io";
const URL_ = process.env.BALQ_URL || "http://127.0.0.1:7928";
const proxy = "0x35825972e2ca90851b14576C531F13dA0B5d53ce";
const user = "0x61Cc0C60bD1cbd6191872fbCA7eeF26f459Fca80";
const abi = JSON.parse(readFileSync(new URL("../../testbed/out/Playground.sol/Playground.json", import.meta.url), "utf8")).abi;

let fails = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) fails++; };

const client = createPublicClient({ transport: balq({ url: URL_, fallback: http(RPC) }) });
const strict = createPublicClient({ transport: balq({ url: URL_, fallback: http(RPC), strict: true }) });

// Served by balq: a mapping getter at a historical block.
const bal = await client.readContract({ address: proxy, abi, functionName: "balances", args: [user], blockNumber: 114591n });
check(bal === 37585n, `readContract balances(user) @114591 = ${bal}`);

// A value getter and a struct getter (tuple).
const counter = await client.readContract({ address: proxy, abi, functionName: "counter", blockNumber: 114591n });
check(counter === 12n, `readContract counter() @114591 = ${counter}`);
const totals = await client.readContract({ address: proxy, abi, functionName: "totals", blockNumber: 114591n });
check(Array.isArray(totals) && totals[1] === 12000000000000000105n, `readContract totals() @114591 = ${totals}`);

// Nested mapping with two keys and an array by index.
const nested = await client.readContract({ address: proxy, abi, functionName: "nested", args: [user, 7n], blockNumber: 114591n });
check(typeof nested === "bigint", `readContract nested(user, 7) = ${nested}`);
const item = await client.readContract({ address: proxy, abi, functionName: "items", args: [1n], blockNumber: 114591n });
check(typeof item === "bigint" && item > 0n, `readContract items(1) = ${String(item).slice(0, 12)}…`);

// Raw storage.
const slot0 = await client.getStorageAt({ address: proxy, slot: "0x0", blockNumber: 114591n });
check(slot0.endsWith("0c"), `getStorageAt slot 0 @114591 = …${slot0.slice(-4)}`);

// Not ours → fallback answers.
const chain = await client.request({ method: "eth_chainId" });
check(BigInt(chain) === 7091047534n, `eth_chainId via fallback = ${BigInt(chain)}`);

// A miss: before the history → default falls back to the node (which also
// cannot serve old state on this gateway → viem throws its own error), strict
// throws balq's typed error.
let strictErr;
try { await strict.getStorageAt({ address: proxy, slot: "0x0", blockNumber: 100n }); } catch (e) { strictErr = e; }
const cause = strictErr && (strictErr instanceof BalqRpcError ? strictErr : strictErr.cause);
check(cause && cause.balqCode === "BeforeStart", `strict miss → BalqRpcError ${cause && cause.balqCode}`);

// balq_getField by name.
const field = await client.request({ method: "balq_getField", params: [proxy, `balances[${user}]`, "0x1bf9f"] });
check(field.value === "37585" && field.provenance === "bal", `balq_getField balances[user] = ${field.value} (${field.provenance})`);

// `latest` is served locally while the archive keeps up with the node.
const latest = await client.readContract({ address: proxy, abi, functionName: "counter" });
check(typeof latest === "bigint", `readContract counter() latest = ${latest}`);

console.log(fails ? `\n${fails} failure(s)` : "\nall ok");
process.exit(fails ? 1 : 0);
