import type { Transport } from "viem";

/** A miss or refusal from `balq serve`, with balq's own code when it is a miss. */
export declare class BalqRpcError extends Error {
  name: "BalqRpcError";
  /** JSON-RPC error code: -32000 miss, -32601 not served by balq, -32602 bad params. */
  code: number;
  /** For a miss: NotWatched | AfterHead | BeforeStart | NotSynced | NeverRecorded | UnknownBefore | Internal. */
  balqCode?: string;
  data?: unknown;
}

export interface BalqTransportOptions {
  /** `balq index --serve` URL. Default `http://127.0.0.1:7928`. */
  url?: string;
  /** Transport for everything balq cannot answer — usually `http(RPC_URL)`. */
  fallback?: Transport;
  /** Throw on a balq miss instead of asking the fallback. Default false. */
  strict?: boolean;
  /** Route only these addresses through balq. Default: try balq for every address. */
  addresses?: readonly string[];
  /** Serve `latest` from balq only while the archive is at most this many blocks behind the node. Default 1; 0 disables. */
  headLag?: number;
}

/**
 * A viem transport: `eth_getStorageAt`, `eth_call` on compiler-generated
 * getters and `balq_getField` are answered by the local archive; everything
 * else — and every miss — goes to `fallback`.
 */
export declare function balq(options?: BalqTransportOptions): Transport;
