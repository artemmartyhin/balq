import { ponder } from "ponder:registry";
import { holder } from "ponder:schema";
import { WETH } from "../abis/WETH";

// A state read at the event's block — the call that normally needs an
// archive node. `context.client` pins it to `event.block.number`; balq
// answers `balanceOf` (the getter of `mapping balanceOf`) from its archive.
ponder.on("WETH:Transfer", async ({ event, context }) => {
  for (const who of [event.args.src, event.args.dst]) {
    const balance = await context.client.readContract({
      abi: WETH,
      address: event.log.address,
      functionName: "balanceOf",
      args: [who],
    });
    await context.db
      .insert(holder)
      .values({ address: who, balance, block: event.block.number })
      .onConflictDoUpdate({ balance, block: event.block.number });
  }
});
