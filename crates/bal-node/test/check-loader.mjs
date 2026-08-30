// The napi loader (index.js) is generated from package.json's name. If the
// package is renamed and index.js is not regenerated, every install fails
// with "Cannot find native binding". Refuse to pack or publish in that state.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const loader = readFileSync(join(root, "index.js"), "utf8");

const missing = Object.keys(pkg.optionalDependencies ?? {}).filter(
  (name) => !loader.includes(`require('${name}')`),
);
if (missing.length) {
  console.error(`index.js does not load: ${missing.join(", ")}\nrun: npm run build`);
  process.exit(1);
}
console.log(`loader ok: ${Object.keys(pkg.optionalDependencies).length} platform package(s)`);
