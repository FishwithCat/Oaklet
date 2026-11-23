import assert from "node:assert/strict";
import test from "node:test";
import { fileParts, matchFiles } from "../src/features/files/quick-open.ts";

const files = [
  "docs/app-guide.md",
  "src/app.tsx",
  "src/components/AppHeader.tsx",
  "src/wrap.ts",
  "README.md",
];

test("splits workspace paths into a filename and folder label", () => {
  assert.deepEqual(fileParts("packages/core/src/agent.rs"), {
    name: "agent.rs",
    dir: "packages/core/src",
  });
  assert.deepEqual(fileParts("README.md"), { name: "README.md", dir: "" });
  assert.deepEqual(fileParts("src\\features\\Files.tsx"), {
    name: "Files.tsx",
    dir: "src/features",
  });
});

test("ranks filename matches above folder matches and ignores case", () => {
  assert.deepEqual(matchFiles(files, "APP"), [
    "docs/app-guide.md",
    "src/app.tsx",
    "src/components/AppHeader.tsx",
  ]);
  assert.deepEqual(matchFiles(files, "components"), ["src/components/AppHeader.tsx"]);
});

test("matches scattered query characters and reports no match", () => {
  assert.deepEqual(matchFiles(files, "apph"), ["src/components/AppHeader.tsx"]);
  assert.deepEqual(matchFiles(files, "zzz"), []);
});

test("returns the first files for an empty query and honors the result limit", () => {
  assert.deepEqual(matchFiles(files, "", 2), ["docs/app-guide.md", "src/app.tsx"]);
  assert.deepEqual(matchFiles(files, "app", 1), ["docs/app-guide.md"]);
});
