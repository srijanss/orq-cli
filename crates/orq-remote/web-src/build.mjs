// Builds the React page into one file the Rust server includes. No network is used:
// everything comes from node_modules. Usage: node build.mjs [--out <file>] [--sw-out <file>]
// When src/sw.js exists it is built too, to web/sw.js (or --sw-out), after the page bundle.
import { build } from "esbuild";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
function option(name, fallback) {
  const at = process.argv.indexOf(name);
  if (at === -1) return fallback;
  if (!process.argv[at + 1]) throw new Error(`${name} needs a file`);
  return resolve(process.argv[at + 1]);
}
const outfile = option("--out", resolve(here, "../web/app.react.js"));
const swOutfile = option("--sw-out", resolve(here, "../web/sw.js"));

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

// Hashes files as their path relative to `root`, a zero byte, the bytes, a zero byte, in path order.
function fingerprint(root, paths) {
  const names = paths.map((path) => [relative(root, path).split("\\").join("/"), path]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  const hash = createHash("sha256");
  for (const [name, path] of names) {
    hash.update(name);
    hash.update(Buffer.from([0]));
    hash.update(readFileSync(path));
    hash.update(Buffer.from([0]));
  }
  return hash.digest("hex");
}

const inputs = ["build.mjs", "package.json", "package-lock.json"].map((name) => join(here, name)).concat(files(join(here, "src")));
const sources = fingerprint(here, inputs);
const bundle = createHash("sha256").update(readFileSync(outfile)).digest("hex");
let stamp = `sources ${sources}\nbundle ${bundle}\n`;

// The service worker caches the app shell under a name holding this version, so a release that changes
// any shell file (everything under web/ but the worker and the stamps) replaces the cache.
const worker = resolve(here, "src/sw.js");
if (existsSync(worker)) {
  const web = resolve(here, "../web");
  const shell = files(web).filter((path) => {
    const name = relative(web, path).split("\\").join("/");
    return name !== "sw.js" && !name.endsWith(".stamp");
  });
  const version = fingerprint(web, shell).slice(0, 16);
  await build({
    entryPoints: [worker],
    outfile: swOutfile,
    bundle: false,
    minify: true,
    format: "iife",
    target: "es2020",
    legalComments: "none",
    define: { __ORQ_SHELL_VERSION__: JSON.stringify(version) },
    logLevel: "warning",
  });
  stamp += `sw ${createHash("sha256").update(readFileSync(swOutfile)).digest("hex")}\n`;
}
writeFileSync(outfile + ".stamp", stamp);
