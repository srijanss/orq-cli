// Builds the React page into one file the Rust server includes. No network is used:
// everything comes from node_modules. Usage: node build.mjs [--out <file>]
import { build } from "esbuild";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const flag = process.argv.indexOf("--out");
const outfile = flag === -1 ? resolve(here, "../web/app.react.js") : resolve(process.argv[flag + 1]);

await build({
  entryPoints: [resolve(here, "src/main.jsx")],
  outfile,
  bundle: true,
  minify: true,
  format: "iife",
  target: "es2020",
  jsx: "automatic",
  legalComments: "none",
  define: { "process.env.NODE_ENV": '"production"' },
  logLevel: "warning",
});

// The stamp lets a test without node notice sources that changed after the last build.
// Inputs are hashed as: relative path, a zero byte, the bytes, a zero byte, in path order.
function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

const inputs = ["build.mjs", "package.json", "package-lock.json"].map((name) => join(here, name)).concat(files(join(here, "src")));
const names = inputs.map((path) => [relative(here, path).split("\\").join("/"), path]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
const sources = createHash("sha256");
for (const [name, path] of names) {
  sources.update(name);
  sources.update(Buffer.from([0]));
  sources.update(readFileSync(path));
  sources.update(Buffer.from([0]));
}
const bundle = createHash("sha256").update(readFileSync(outfile)).digest("hex");
writeFileSync(outfile + ".stamp", `sources ${sources.digest("hex")}\nbundle ${bundle}\n`);
