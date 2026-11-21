import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "..", "dist");
const compileEntry = join(dist, "src", "compile.mjs");
const mcpEntry = join(dist, "src", "mcp.mjs");
const fixtures = join(here, "fixtures");
const built = existsSync(compileEntry) && existsSync(mcpEntry);

function run(entry, args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [entry, ...args], { timeout: 20_000 });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      try {
        resolve({ code, stderr, response: JSON.parse(stdout) });
      } catch {
        reject(
          new Error(
            `Helper returned invalid JSON (exit ${code}): ${stdout}\n${stderr}`,
          ),
        );
      }
    });
    child.stdin.end(JSON.stringify(input));
  });
}

async function validSources() {
  const dir = join(fixtures, "valid", "architecture");
  const sources = {};
  for (const file of await readdir(dir)) {
    sources[`architecture/${file}`] = await readFile(join(dir, file), "utf8");
  }
  return sources;
}

test(
  "the bundled compiler lays out a multi-file model",
  { skip: !built && "run `pnpm build` first" },
  async () => {
    const { code, response } = await run(compileEntry, [], {
      version: 1,
      sources: await validSources(),
    });
    assert.equal(code, 0);
    assert.equal(response.ok, true);
    assert.equal(response.projectId, "oaklet-fixture");
    assert.equal(response.model._stage, "layouted");
  },
);

test(
  "the bundled MCP bridge still loads a TypeScript config through jiti",
  { skip: !built && "run `pnpm build` first" },
  async () => {
    const workspace = await mkdtemp(join(tmpdir(), "oaklet-bundle-config-"));
    try {
      await mkdir(join(workspace, "architecture"));
      await writeFile(
        join(workspace, "likec4.config.ts"),
        [
          'import { defineConfig } from "@likec4/config";',
          'export default defineConfig({ name: "bundle-config" });',
          "",
        ].join("\n"),
      );
      await writeFile(
        join(workspace, "architecture", "model.c4"),
        [
          "specification {",
          "  element system",
          "}",
          "model {",
          "  sys = system 'System'",
          "}",
          "views {",
          "  view index {",
          "    include *",
          "  }",
          "}",
          "",
        ].join("\n"),
      );

      const { code, response } = await run(
        mcpEntry,
        [workspace],
        { tool: "list-projects" },
      );
      assert.equal(code, 0);
      assert.equal(
        response.structuredContent.projects[0].id,
        "bundle-config",
      );
    } finally {
      await rm(workspace, { recursive: true, force: true });
    }
  },
);
