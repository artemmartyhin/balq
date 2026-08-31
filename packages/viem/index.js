// @balq/viem — a viem transport that answers reads of your contracts from a
// local balq archive and sends everything else to your usual RPC.
//
//   import { createPublicClient, http } from "viem";
//   import { balq } from "@balq/viem";
//
//   const client = createPublicClient({
//     chain,
//     transport: balq({ fallback: http(RPC_URL) }),
//   });
//
//   await client.readContract({ address, abi, functionName: "balanceOf", args: [user], blockNumber })
//   // → served by balq when `balanceOf` is the getter of a variable in the
//   //   contract's layout and the block is in the archive; otherwise by RPC.
//
// What balq answers: `eth_getStorageAt`, `eth_call` on compiler-generated
// getters (public variables: values, mappings, arrays, structs), and
// `balq_getField` (any variable by name, private ones included). What it
// does not: view functions with logic, other contracts, logs, transactions,
// `pending` — those go to `fallback` untouched.

import { custom } from "viem";

/** Methods balq may answer. Everything else goes straight to the fallback. */
const LOCAL = new Set(["eth_getStorageAt", "eth_call", "balq_getField", "balq_status", "eth_blockNumber"]);

class BalqRpcError extends Error {
  constructor(method, err) {
    super(`${method}: ${err.message}`);
    this.name = "BalqRpcError";
    this.code = err.code;
    /** balq's own code for a miss: NotWatched | AfterHead | BeforeStart | NeverRecorded | UnknownBefore … */
    this.balqCode = err.data && err.data.code;
    this.data = err.data;
  }
}

async function call(url, method, params, id) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
  });
  if (!res.ok) throw new Error(`balq serve ${url}: HTTP ${res.status}`);
  const body = await res.json();
  if (body.error) throw new BalqRpcError(method, body.error);
  return body.result;
}

/**
 * @param {object} [options]
 * @param {string} [options.url]        balq serve URL (default http://127.0.0.1:7928)
 * @param {import("viem").Transport} [options.fallback]
 *        transport for everything balq cannot answer (usually `http(RPC_URL)`)
 * @param {boolean} [options.strict]    never fall back on a balq *miss*: throw
 *        BalqRpcError with `.balqCode` instead (default false — a miss goes to
 *        the fallback, so the caller still gets an answer, from the node)
 * @param {string[]} [options.addresses] only route calls to these addresses
 *        through balq (default: try balq for every address; NotWatched falls back)
 * @param {number} [options.headLag]    with `latest`, serve from balq only when the
 *        archive is at most this many blocks behind the node (default 1; needs
 *        `fallback` to learn the node's head; 0 disables the check)
 * @returns {import("viem").Transport}
 */
export function balq(options = {}) {
  const url = (options.url || "http://127.0.0.1:7928").replace(/\/$/, "");
  const strict = options.strict === true;
  const only = options.addresses ? new Set(options.addresses.map((a) => a.toLowerCase())) : null;
  const headLag = options.headLag ?? 1;
  let seq = 0;

  return (cfg) => {
    const up = options.fallback ? options.fallback(cfg) : null;
    const fallback = (method, params) => {
      if (!up) throw new Error(`balq transport: cannot answer ${method} and no fallback transport is configured`);
      return up.request({ method, params });
    };
    const addressOf = (method, params) => {
      if (method === "eth_getStorageAt" || method === "balq_getField") return params && params[0];
      if (method === "eth_call") return params && params[0] && params[0].to;
      return null;
    };
    const blockOf = (method, params) => {
      if (method === "eth_getStorageAt" || method === "balq_getField") return params && params[2];
      if (method === "eth_call") return params && params[1];
      return "latest";
    };

    const request = async ({ method, params }) => {
      if (!LOCAL.has(method)) return fallback(method, params);
      const addr = addressOf(method, params);
      if (only && addr && !only.has(String(addr).toLowerCase())) return fallback(method, params);
      const tag = blockOf(method, params);
      if (tag === "pending") return fallback(method, params);

      // `latest` is honest only while the archive is (nearly) at the node's head.
      if ((tag === undefined || tag === null || tag === "latest") && headLag > 0 && up && method !== "eth_blockNumber") {
        try {
          const [mine, theirs] = await Promise.all([call(url, "eth_blockNumber", [], ++seq), up.request({ method: "eth_blockNumber", params: [] })]);
          if (Number(BigInt(theirs) - BigInt(mine)) > headLag) return fallback(method, params);
        } catch {
          return fallback(method, params);
        }
      }

      try {
        return await call(url, method, params || [], ++seq);
      } catch (e) {
        if (e instanceof BalqRpcError) {
          // -32601: not a getter / no layout — never ours. -32000: a miss.
          if (strict && e.code === -32000) throw e;
          if (up && (e.code === -32601 || e.code === -32000)) return fallback(method, params);
        }
        if (up && !(e instanceof BalqRpcError)) return fallback(method, params); // serve not reachable
        throw e;
      }
    };

    return custom({ request }, { key: "balq", name: "balq", retryCount: 0 })(cfg);
  };
}

export { BalqRpcError };
