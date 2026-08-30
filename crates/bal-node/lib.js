// Thin wrapper over the generated `index.js`:
//  - turns `[NotAvailable:Code] …` errors from Rust into `NotAvailableError`
//    with a `.code`;
//  - adds `archive.view(address, layout).at(block)`, a proxy that reads the
//    contract's variables by name, the way they are written in Solidity.
// No archive or layout logic lives here; every read goes through the native
// `locate` / `kindOf` / `storageAt` / `decodeValue`.

const native = require("./index.js");

class NotAvailableError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "NotAvailableError";
    this.code = code;
  }
}

const RE = /^\[NotAvailable:([A-Za-z]+)\] (.*)$/s;

function convert(e) {
  const m = typeof e?.message === "string" && RE.exec(e.message);
  return m ? new NotAvailableError(m[1], m[2]) : e;
}

function wrap(obj, names) {
  for (const n of names) {
    const orig = obj[n];
    obj[n] = function (...args) {
      try {
        const r = orig.apply(this, args);
        return r instanceof Promise ? r.catch((e) => { throw convert(e); }) : r;
      } catch (e) {
        throw convert(e);
      }
    };
  }
}

wrap(native.Archive.prototype, ["storageAt", "history", "changedSlots", "sync", "backfill", "backfillMany", "bootstrapSlot"]);

// ---- view -----------------------------------------------------------------

function toJs(decoded) {
  switch (decoded.kind) {
    case "uint":
    case "int":
      return BigInt(decoded.text);
    case "bool":
      return decoded.text === "true";
    default:
      return decoded.text; // address, bytes, raw: 0x-hex; string: the text
  }
}

// Properties JS itself probes on any object; never storage fields.
const PASSTHROUGH = new Set(["then", "toJSON", "constructor", "valueOf", "toString", "inspect"]);

function readLeaf(archive, address, layout, block, path, kind) {
  const loc = layout.locate(path);
  const v = archive.storageAt(address, loc.slot, block);
  if (kind === "value:string" || kind === "value:dynbytes") {
    // Long values live in further slots; read them at the same block.
    const chunks = layout.bytesDataSlots(loc, v.value).map((s) => archive.storageAt(address, s, block).value);
    return toJs(layout.decodeBytes(loc, v.value, chunks));
  }
  return toJs(layout.decodeValue(loc, v.value));
}

/**
 * A proxy for the container at `path` ("" = the contract itself).
 * Property access extends the path according to the container's kind and
 * either descends (struct / mapping / array) or reads a leaf value.
 */
function container(archive, address, layout, block, path, kind) {
  const extend = (prop) => {
    if (kind === "struct" || kind === "") return path ? `${path}.${prop}` : prop;
    if (kind === "array" && prop === "length") return `${path}.length`;
    return `${path}[${prop}]`; // mapping, array, fixedArray
  };
  return new Proxy(Object.create(null), {
    get(_, prop) {
      if (typeof prop === "symbol" || PASSTHROUGH.has(prop)) return undefined;
      const next = extend(String(prop));
      const k = layout.kindOf(next); // throws for unknown fields
      if (k.startsWith("value:")) return readLeaf(archive, address, layout, block, next, k);
      return container(archive, address, layout, block, next, k);
    },
    has(_, prop) {
      if (typeof prop === "symbol") return false;
      try {
        layout.kindOf(extend(String(prop)));
        return true;
      } catch {
        return false;
      }
    },
    ownKeys() {
      return kind === "" || !path ? layout.fields() : [];
    },
    getOwnPropertyDescriptor(_, prop) {
      return { enumerable: true, configurable: true, value: undefined };
    },
  });
}

/**
 * `archive.view(address, layout)` → `{ at(block) }`.
 * `at(block)` returns an object whose properties are the contract's storage
 * variables at the end of `block`: `view.counter`, `view.balances[addr]`,
 * `view.totals.index`, `view.items[3]`, `view.items.length`.
 * Integers are `bigint`, bools `boolean`, addresses/bytes/raw words `string`.
 * A missing value throws `NotAvailableError` — never `undefined`.
 */
native.Archive.prototype.view = function (address, layout) {
  const archive = this;
  return {
    at(block) {
      return container(archive, address, layout, block, "", "");
    },
  };
};

module.exports = { ...native, NotAvailableError };
