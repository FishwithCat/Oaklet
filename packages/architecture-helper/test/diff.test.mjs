import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { fromSources } from "@likec4/language-services/node";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "fixtures");

async function validSources() {
  const dir = join(fixtures, "valid", "architecture");
  return {
    "architecture/likec4.config.json": await readFile(
      join(dir, "likec4.config.json"),
      "utf8",
    ),
    "architecture/model.c4": await readFile(join(dir, "model.c4"), "utf8"),
    "architecture/views.c4": await readFile(join(dir, "views.c4"), "utf8"),
  };
}

async function modelWithDrafts(drafts) {
  return fromSources(
    { ...(await validSources()), ...drafts },
    {
      graphviz: "wasm",
      printErrors: false,
      throwIfInvalid: false,
      configureLogger: false,
    },
  );
}

const workerDraft = `diff worker 'Add background worker' {
  description 'Introduce a queue in front of the worker'
  add {
    extend api {
      queue = component 'Queue'
    }
    api.web -> api.queue 'Enqueues'
  }
}
`;

test("reads a native diff from an in-memory draft over a multi-file model", async () => {
  const likec4 = await modelWithDrafts({
    "architecture/draft.c4": workerDraft,
  });
  try {
    assert.deepEqual(likec4.getErrors(), []);
    assert.deepEqual(likec4.getWarnings(), []);

    const [diff] = await likec4.modelDiffs.list();
    assert.equal(diff.id, "worker");
    assert.equal(diff.title, "Add background worker");
    assert.deepEqual(diff.added, { elements: 1, relations: 1 });
    assert.deepEqual(diff.removed, { elements: 0, relations: 0 });
    assert.deepEqual(diff.modified, { elements: 0 });
    assert.deepEqual(diff.views, [
      {
        id: "containers",
        title: "API containers",
        nodes: { added: 1, removed: 0, modified: 0 },
        edges: { added: 1, removed: 0, modified: 0 },
      },
    ]);
  } finally {
    await likec4.dispose();
  }
});

test("returns diagnostics for a diff that references an unknown element", async () => {
  const likec4 = await modelWithDrafts({
    "architecture/draft.c4": `diff broken 'Broken' {
  add {
    api.web -> api.missing 'Calls'
  }
}
`,
  });
  try {
    const diagnostics = likec4.getErrors();
    assert.ok(diagnostics.length > 0);
    assert.ok(
      diagnostics.every((diagnostic) =>
        diagnostic.sourceFsPath.endsWith("architecture/draft.c4"),
      ),
    );
    assert.ok(
      diagnostics.some((diagnostic) =>
        diagnostic.message.includes("'missing'"),
      ),
    );
    assert.ok(diagnostics[0].line >= 0);
    assert.ok(diagnostics[0].range.start.line >= 0);
  } finally {
    await likec4.dispose();
  }
});

test("reports overlapping drafts as warnings without breaking the model", async () => {
  const likec4 = await modelWithDrafts({
    "architecture/draft.c4": `diff first 'First' {
  remove {
    api.worker -> api.store 'Reads and writes'
  }
}

diff second 'Second' {
  remove {
    api.worker -> api.store 'Reads and writes'
  }
}
`,
  });
  try {
    assert.deepEqual(likec4.getErrors(), []);
    const warnings = likec4.getWarnings();
    assert.equal(warnings.length, 2);
    assert.ok(warnings[0].message.includes("Overlaps with diff"));
    assert.deepEqual(likec4.modelDiffs.checkBeforeApply("first"), {
      errors: [],
      overlaps: ["second: both remove api.worker -> api.store 'Reads and writes'"],
    });

    const diffs = await likec4.modelDiffs.list();
    assert.deepEqual(
      diffs.map((diff) => diff.id),
      ["first", "second"],
    );
    for (const diff of diffs) {
      assert.deepEqual(diff.removed, { elements: 0, relations: 1 });
      assert.deepEqual(diff.views[0].edges, {
        added: 0,
        removed: 1,
        modified: 0,
      });
    }
  } finally {
    await likec4.dispose();
  }
});

test("lays out a compare view with the diff statuses the desktop consumes", async () => {
  const likec4 = await modelWithDrafts({
    "architecture/draft.c4": workerDraft,
  });
  try {
    const view = await likec4.modelDiffs.layoutCompareView({
      viewId: "containers",
      diffId: "worker",
    });
    assert.equal(view._stage, "layouted");
    assert.equal(view._type, "element");

    const queue = view.nodes.find((node) => node.id === "api.queue");
    assert.equal(queue.title, "Queue");
    assert.equal(queue.diffStatus, "added");
    const enqueues = view.edges.find((edge) => edge.target === "api.queue");
    assert.equal(enqueues.label, "Enqueues");
    assert.equal(enqueues.diffStatus, "added");
  } finally {
    await likec4.dispose();
  }
});
