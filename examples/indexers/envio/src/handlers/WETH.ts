import { indexer, createEffect, S } from "envio";
import { createPublicClient, http, parseAbi } from "viem";
import { balq } from "@balq/viem";

// Historical state for handlers: the viem client reads storage-backed
// getters from `balq index … --serve` and sends anything else to the node.
// (`http(BALQ_URL)` alone works too when balq runs with --upstream.)
const client = createPublicClient({
  transport: balq({
    url: process.env.BALQ_URL ?? "http://127.0.0.1:7928",
    fallback: http(process.env.RPC_URL),
  }),
});

const abi = parseAbi(["function balanceOf(address) view returns (uint256)"]);

const balanceAt = createEffect(
  {
    name: "balanceAt",
    input: { token: S.address, who: S.address, block: S.number },
    output: S.bigint,
    rateLimit: false,
    cache: true, // a balance at a past block never changes
  },
  async ({ input }) =>
    client.readContract({
      address: input.token,
      abi,
      functionName: "balanceOf",
      args: [input.who],
      blockNumber: BigInt(input.block),
    }),
);

indexer.onEvent({ contract: "WETH", event: "Transfer" }, async ({ event, context }) => {
  for (const who of [event.params.src, event.params.dst]) {
    const balance = await context.effect(balanceAt, {
      token: event.srcAddress,
      who,
      block: event.block.number,
    });
    context.Holder.set({ id: who, balance, block: event.block.number });
  }
});
