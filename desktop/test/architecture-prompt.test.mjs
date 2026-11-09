import assert from "node:assert/strict";
import test from "node:test";
import {
  buildArchitecturePrompt,
  mergeDraft,
} from "../src/architecture-prompt.ts";

test("a create prompt targets the default model and requires source evidence", () => {
  const prompt = buildArchitecturePrompt({
    mode: "create",
    modelRoot: ".",
    modelPaths: [],
    defaultPath: "architecture/model.c4",
  });

  assert.match(prompt, /Create a LikeC4 architecture model/);
  assert.match(prompt, /architecture\/model\.c4/);
  assert.match(prompt, /context and container views/);
  assert.match(prompt, /source evidence/i);
  assert.match(prompt, /Make only architecture-file edits/);
  assert.match(prompt, /never claim validation ran if it did not/i);
  assert.ok(prompt.length < 6_000);
});

test("an update prompt targets the selected files and includes diagnostics", () => {
  const prompt = buildArchitecturePrompt({
    mode: "update",
    modelRoot: "architecture",
    modelPaths: [
      "architecture/model.c4",
      "architecture/views.c4",
      "architecture/deployment.c4",
    ],
    defaultPath: "architecture/model.c4",
    diagnostics: [
      {
        message: "Target not resolved\nat relation",
        file: "architecture/model.c4",
        line: 7,
        column: 3,
      },
    ],
  });

  assert.match(prompt, /Update the existing LikeC4 architecture model/);
  assert.match(prompt, /architecture\/model\.c4/);
  assert.match(prompt, /architecture\/views\.c4/);
  assert.match(prompt, /architecture\/deployment\.c4/);
  assert.match(prompt, /architecture\/model\.c4:7:3: Target not resolved at relation/);
  assert.match(prompt, /Preserve stable element IDs/);
});

test("the prompt lists only a bounded set of model paths", () => {
  const paths = Array.from(
    { length: 40 },
    (_, index) => `architecture/part-${index}.c4`,
  );
  const prompt = buildArchitecturePrompt({
    mode: "update",
    modelRoot: "architecture",
    modelPaths: paths,
    defaultPath: "architecture/model.c4",
  });

  assert.match(prompt, /architecture\/part-19\.c4/);
  assert.doesNotMatch(prompt, /architecture\/part-20\.c4/);
  assert.match(prompt, /and 20 more source files/);
});

test("merging a prompt preserves an existing composer draft", () => {
  assert.equal(mergeDraft("", "generated"), "generated");
  assert.equal(mergeDraft("my notes\n\n", "generated"), "my notes\n\ngenerated");
  assert.equal(
    mergeDraft("my notes\n\ngenerated", "generated"),
    "my notes\n\ngenerated",
  );
  assert.equal(mergeDraft("  ", "generated"), "generated");
});
