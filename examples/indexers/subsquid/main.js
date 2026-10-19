// A Squid whose RPC endpoint is `balq index … --serve --upstream`.
//
//   balq index 0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9 --rpc $SEPOLIA_RPC \
//       --layout auto --serve --upstream
//   FROM=11936100 TO=11936300 node main.js
//
// The processor ingests blocks and logs through the relay as usual; the
// handler's `balanceOf` at each event's block is an `eth_call` balq answers
// from its verified archive — the call that otherwise needs an archive node.
// A real squid keeps its TypeormDatabase; the in-memory store below only
// keeps the example free of Postgres.

const { EvmBatchProcessor } = require("@subsquid/evm-processor");
const { event, func, indexed, ContractBase } = require("@subsquid/evm-abi");
const p = require("@subsquid/evm-codec");

const WETH = "0x7b79995e5f793a07bc00c21412e50ecae098e7f9";
const BALQ = process.env.BALQ_URL || "http://127.0.0.1:7928";

// What `squid-evm-typegen` generates from the ABI, written out by hand.
const Transfer = event("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef", {
  src: indexed(p.address),
  dst: indexed(p.address),
  wad: p.uint256,
});
const balanceOf = func("0x70a08231", { _0: p.address }, p.uint256);
class Weth extends ContractBase {
  balanceOf(who) {
    return this.eth_call(balanceOf, { _0: who });
  }
}

const processor = new EvmBatchProcessor()
  .setRpcEndpoint({ url: BALQ, rateLimit: 50 })
  .setFinalityConfirmation(10)
  .setBlockRange({ from: Number(process.env.FROM || 11936100), to: Number(process.env.TO || 11936300) })
  .addLog({ address: [WETH], topic0: [Transfer.topic] })
  .setFields({ log: { topics: true, data: true } });

/** `FinalDatabase` in memory: a map of balances. */
const balances = new Map();
let head = { height: -1, hash: "0x" };
const db = {
  async connect() {
    return head;
  },
  async transact(info, cb) {
    await cb(balances);
    head = info.nextHead;
  },
};

processor.run(db, async (ctx) => {
  for (const block of ctx.blocks) {
    for (const log of block.logs) {
      if (log.topics[0] !== Transfer.topic) continue;
      const { src, dst } = Transfer.decode(log);
      const weth = new Weth(ctx, block.header, WETH);
      for (const who of [src, dst]) {
        ctx.store.set(who, { balance: await weth.balanceOf(who), block: block.header.height });
      }
    }
  }
  ctx.log.info(`${ctx.store.size} holders, up to block ${ctx.blocks.at(-1)?.header.height}`);
  if (process.env.OUT) require("fs").writeFileSync(process.env.OUT, JSON.stringify([...ctx.store].map(([a, v]) => ({ address: a, balance: String(v.balance), block: v.block }))));
});
