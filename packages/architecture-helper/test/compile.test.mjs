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

function compile(sources, diff) {
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
    child.stdin.end(JSON.stringify({ version: 1, sources, ...(diff ? { diff } : {}) }));
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

test("validates one native diff draft and returns its summary", async () => {
  const { response } = await compile(await sourcesFor("valid"), {
    path: "architecture/.oaklet-plan-1.c4",
    text: `diff worker 'Add background worker' {
  description 'Introduce a queue in front of the worker'
  add {
    extend api {
      queue = component 'Queue'
    }
    api.web -> api.queue 'Enqueues'
  }
}
`,
  });

  assert.equal(response.ok, true);
  assert.equal(response.diff.id, "worker");
  assert.equal(response.diff.title, "Add background worker");
  assert.equal(response.diff.description, "Introduce a queue in front of the worker");
  assert.deepEqual(response.diff.added, { elements: 1, relations: 1 });
  assert.deepEqual(response.diff.views[0].nodes, {
    added: 1,
    removed: 0,
    modified: 0,
  });
  assert.deepEqual(response.overlaps, []);
});

test("rejects a draft with more than one top-level diff", async () => {
  const { response } = await compile(await sourcesFor("valid"), {
    path: "architecture/.oaklet-plan-2.c4",
    text: `diff first 'First' {
  add { extend api { first = component 'First' } }
}

diff second 'Second' {
  add { extend api { second = component 'Second' } }
}
`,
  });

  assert.equal(response.ok, false);
  assert.equal(response.draftError, true);
  assert.match(response.error, /exactly one top-level diff/);
});

test("rejects a draft whose ID already exists in the model", async () => {
  const sources = await sourcesFor("valid");
  sources["architecture/existing.c4"] = `diff worker 'Existing worker' {
  add { extend api { existing = component 'Existing' } }
}
`;
  const { response } = await compile(sources, {
    path: "architecture/.oaklet-plan-3.c4",
    text: `diff worker 'Another worker' {
  add { extend api { another = component 'Another' } }
}
`,
  });

  assert.equal(response.ok, false);
  assert.equal(response.draftError, true);
  assert.match(response.error, /already exists/);
});

test("reports overlapping drafts as warnings without failing validation", async () => {
  const sources = await sourcesFor("valid");
  sources["architecture/existing.c4"] = `diff existing 'Existing removal' {
  remove { api.worker -> api.store 'Reads and writes' }
}
`;
  const { response } = await compile(sources, {
    path: "architecture/.oaklet-plan-4.c4",
    text: `diff worker 'Remove the same relation' {
  remove { api.worker -> api.store 'Reads and writes' }
}
`,
  });

  assert.equal(response.ok, true);
  assert.equal(response.diff.id, "worker");
  assert.equal(response.overlaps.length, 1);
  assert.match(response.overlaps[0], /^existing: both remove/);
  assert.ok(response.warnings.some((warning) => /Overlaps with diff/.test(warning.message)));
});

test("rejects unsafe or colliding draft paths", async () => {
  const sources = await sourcesFor("valid");
  for (const path of ["../escape.c4", "/absolute.c4", "architecture\\draft.c4", "architecture/model.c4"]) {
    const { response } = await compile(sources, { path, text: "diff x 'X' { }\n" });
    assert.equal(response.ok, false);
    assert.match(response.error, /Unsafe architecture diff path|collides with a source/);
  }
});
