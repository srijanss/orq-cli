// Builds the React page into one file the Rust server includes. No network is used:
// everything comes from node_modules. Usage: node build.mjs [--out <file>]
import { build } from "esbuild";
import { dirname, resolve } from "node:path";
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
