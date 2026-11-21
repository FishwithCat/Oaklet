#!/usr/bin/env node
/**
 * Builds the self-contained helper bundles that Tauri ships.
 *
 * The helper used to be packaged as `node_modules`, which added ~90 MB to the
 * app. Both entrypoints are now bundled with esbuild. `jiti/static` must be
 * used instead of `jiti` so TypeScript LikeC4 configs still load from inside a
 * bundle; plain jiti resolves its Babel transform relative to `import.meta.url`
 * and silently falls back to the default project config when bundled.
 */
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const scripts = dirname(fileURLToPath(import.meta.url));
const helper = join(scripts, "..");
const dist = join(helper, "dist");
const packageJson = JSON.parse(await readFile(join(helper, "package.json"), "utf8"));

await rm(dist, { recursive: true, force: true });
await mkdir(dist, { recursive: true });

const result = await build({
  absWorkingDir: helper,
  entryPoints: ["src/compile.mjs", "src/mcp.mjs"],
  outdir: join(dist, "src"),
  bundle: true,
  splitting: true,
  platform: "node",
  format: "esm",
  target: "node22",
  minify: true,
  legalComments: "eof",
  // jiti's normal entrypoint loads dist/babel.cjs relative to import.meta.url,
  // which points at our output directory. The static entrypoint inlines it.
  alias: { jiti: "jiti/static" },
  outExtension: { ".js": ".mjs" },
  // Several transitive dependencies are CommonJS and call require() for Node
  // builtins. esbuild's ESM output cannot provide that without a real require.
  banner: {
    js: 'import { createRequire } from "node:module"; const require = createRequire(import.meta.url);',
  },
  metafile: true,
  logLevel: "info",
});

await writeFile(
  join(dist, "package.json"),
  `${JSON.stringify(
    {
      name: packageJson.name,
      version: packageJson.version,
      private: true,
      type: "module",
    },
    null,
    2,
  )}\n`,
);

const files = Object.keys(result.metafile.outputs)
  .map((file) => relative(dist, join(helper, file)))
  .sort();

await writeFile(
  join(dist, "bundle.json"),
  `${JSON.stringify(
    {
      format: 1,
      helperVersion: packageJson.version,
      entries: ["src/compile.mjs", "src/mcp.mjs"],
      files,
    },
    null,
    2,
  )}\n`,
);

console.log(`Helper bundles written to ${relative(helper, dist)}`);
