import { createConfig } from "ponder";
import { WETH } from "./abis/WETH";

// The only line that differs from a stock Ponder app: the chain's RPC is
// `balq index … --serve --upstream`. Historical `context.client` reads of
// indexed contracts come from the verified archive; logs, blocks and
// everything else are relayed to the node behind it — a full node, no
// archive node.
export default createConfig({
  chains: {
    sepolia: {
      id: 11155111,
      rpc: process.env.BALQ_URL ?? "http://127.0.0.1:7928",
    },
  },
  contracts: {
    WETH: {
      chain: "sepolia",
      abi: WETH,
      address: "0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9",
      // Inside the archive's history (`balq status`); before it, reads
      // fall through to the node, which needs archive state for them.
      startBlock: Number(process.env.START_BLOCK ?? 11_936_100),
      endBlock: process.env.END_BLOCK ? Number(process.env.END_BLOCK) : undefined,
    },
  },
});
