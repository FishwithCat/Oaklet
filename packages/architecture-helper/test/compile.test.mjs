import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const helper = join(here, "..", "src", "compile.mjs");
const fixtures = join(here, "fixtures");

async function sourcesFor(name) {
  const dir = join(fixtures, name, "architecture");
  const sources = {};
  for (const file of await readdir(dir)) {
    sources[`architecture/${file}`] = await readFile(join(dir, file), "utf8");
  }
  return sources;
}

function compile(sources) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [helper], {
      stdio: ["pipe", "pipe", "pipe"],
    });
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
    child.stdin.end(JSON.stringify({ version: 1, sources }));
  });
}

test("compiles a multi-file model into a layouted model with views", async () => {
  const { code, response } = await compile(await sourcesFor("valid"));

  assert.equal(code, 0);
  assert.equal(response.ok, true);
  assert.equal(response.projectId, "oaklet-fixture");
  assert.equal(response.model._stage, "layouted");
  assert.deepEqual(
    response.views.map((view) => view.id),
    ["index", "containers"],
  );
  assert.equal(response.views[0].title, "System context");
  assert.equal(response.model.views.index._type, "element");
});

test("returns file and line diagnostics for a malformed model", async () => {
  const { response } = await compile(await sourcesFor("malformed"));

  assert.equal(response.ok, false);
  assert.equal(response.model, undefined);
  assert.ok(response.diagnostics.length > 0);
  assert.equal(response.diagnostics[0].file, "architecture/model.c4");
  assert.equal(response.diagnostics[0].line, 7);
  assert.ok(response.diagnostics[0].message.length > 0);
  assert.ok(response.diagnostics[0].message.length <= 2_000);
});

test("an empty model compiles to a default landscape view", async () => {
  const { response } = await compile(await sourcesFor("empty"));

  assert.equal(response.ok, true);
  assert.equal(response.model._stage, "layouted");
  assert.equal(response.model.views.index._type, "element");
});

test("rejects source paths that escape the virtual workspace", async () => {
  const { response } = await compile({ "../escape.c4": "model { }" });

  assert.equal(response.ok, false);
  assert.match(response.error, /Unsafe architecture source path/);
});
