// Smoke test against the test-bed archive produced by testbed/poke.mjs.
// Expects testbed/balq.redb, testbed/journal.jsonl, testbed/deploy.json.
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { Archive, Layout, NotAvailableError } = require("../lib.js");

const here = path.dirname(fileURLToPath(import.meta.url));
const testbed = path.resolve(here, "../../../testbed");
const deploy = JSON.parse(fs.readFileSync(path.join(testbed, "deploy.json"), "utf8"));
const proxy = deploy.proxy;

let failed = 0;
const check = (cond, msg) => { console.log(`${cond ? "ok  " : "FAIL"} ${msg}`); if (!cond) failed++; };

const ar = Archive.open(path.join(testbed, "balq.redb"));
const head = ar.head();
check(head && head.number > deploy.block, `head ${head?.number}`);
check(ar.watchlist().some((w) => w.address.toLowerCase() === proxy.toLowerCase()), "proxy is watched");

// journal replay = verify, from JS
const rows = fs.readFileSync(path.join(testbed, "journal.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
let match = 0, mismatch = 0, na = {};
for (const r of rows) {
  try {
    const v = ar.storageAt(r.address, r.slot, r.block);
    if (v.value.toLowerCase() === r.value.toLowerCase()) match++; else { mismatch++; console.log("MISMATCH", r); }
  } catch (e) {
    if (e instanceof NotAvailableError) na[e.code] = (na[e.code] ?? 0) + 1; else throw e;
  }
}
check(mismatch === 0 && match === rows.length, `journal: ${match} match, ${mismatch} mismatch, ${JSON.stringify(na)} not available`);

// typed errors, never null
for (const [args, code] of [
  [["0x000000000000000000000000000000000000dEaD", "0", head.number], "NotWatched"],
  [[proxy, "0", deploy.block], "BeforeStart"],
  [[proxy, "0", head.number + 1000], "AfterHead"],
  [[proxy, "0x1234", head.number], "NeverRecorded"],
]) {
  let got = "no error";
  try { ar.storageAt(...args); } catch (e) { got = e instanceof NotAvailableError ? e.code : `other: ${e.message}`; }
  check(got === code, `storageAt(${args.join(", ")}) -> ${got}`);
}

// layout
const layout = Layout.fromFile(path.join(testbed, "Playground.layout.json"));
check(layout.fields().join(",") === "counter,a,b,c,totals,balances,nested,items,lastPoker", `fields: ${layout.fields().join(",")}`);
const last = rows[rows.length - 1].block;
const counter = layout.decode(layout.locate("counter"), ar.storageAt(proxy, layout.locate("counter").slot, last).value);
check(counter === "12", `counter @${last} = ${counter}`);
const bal = layout.locate(`balances[${rows[3].address === proxy ? rows[3].field.slice(9, -1) : ""}]`);
const balV = layout.decode(bal, ar.storageAt(proxy, bal.slot, last).value);
check(balV === "37585", `balances[sender] @${last} = ${balV}`);
const h = ar.history(proxy, "0", deploy.block + 1, last + 1);
check(h.length === 12 && h.every((e) => e.provenance === "bal"), `history(counter): ${h.length} entries`);
const named = layout.describeSlot("0x1").map((n) => n.name).join(",");
check(named === "a,b,c", `describeSlot(1) = ${named}`);
const changed = ar.changedSlots(proxy, h[0].block);
check(changed.length === 8, `changedSlots(${h[0].block}) = ${changed.length}`);

// async sync (no bootstrap; endpoint has proof window 0) — reads keep working meanwhile
const rpc = fs.readFileSync(path.join(testbed, ".env"), "utf8").match(/RPC=(.*)/)[1].trim();
const p = ar.sync(rpc, false);
const during = ar.storageAt(proxy, "0", last);
check(during.value.endsWith("0c"), `read during sync: counter = ${parseInt(during.value, 16)}`);
const rep = await p;
check(typeof rep.blocksApplied === "number", `sync: ${rep.blocksApplied} blocks, head now ${ar.head().number}`);

// view: read by name, like Solidity
const v = ar.view(proxy, layout).at(last);
const sender = rows[3].field.slice(9, -1);
check(v.counter === 12n, `view.counter = ${v.counter}`);
check(v.balances[sender] === 37585n, `view.balances[sender] = ${v.balances[sender]}`);
check(typeof v.totals.index === "bigint" && v.totals.index === 12n * 10n ** 18n + (v.totals.index % 1000n), `view.totals.index = ${v.totals.index}`);
check(v.items.length === 8n, `view.items.length = ${v.items.length}`);
check(typeof v.items[3] === "bigint", `view.items[3] is bigint`);
check(typeof v.c === "boolean", `view.c = ${v.c}`);
check(v.lastPoker.toLowerCase() === sender.toLowerCase(), `view.lastPoker = ${v.lastPoker}`);
check(typeof v.nested[sender][12n] === "bigint", `view.nested[sender][12] is bigint`);
check("counter" in v && !("nope" in v), `'in' reflects the layout`);
let viewErr = "no error";
try { ar.view(proxy, layout).at(deploy.block).counter; } catch (e) { viewErr = e instanceof NotAvailableError ? e.code : e.message; }
check(viewErr === "BeforeStart", `view before start -> ${viewErr}`);
let unknown = "no error";
try { v.nope; } catch (e) { unknown = e.message; }
check(/unknown field/.test(unknown), `unknown field throws: ${unknown.slice(0, 40)}`);

// typegen
const ts = layout.typescript("PlaygroundView");
check(ts.includes("readonly balances: { readonly [key: string]: bigint };"), "typescript() emits mapping type");

console.log(failed ? `\n${failed} FAILED` : "\nall ok");
process.exit(failed ? 1 : 0);
