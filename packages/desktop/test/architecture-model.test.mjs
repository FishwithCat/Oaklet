import assert from "node:assert/strict";
import test from "node:test";
import {
  diagnosticLocation,
  isCurrentLoad,
  pickModelRoot,
  pickViewId,
  rootLabel,
} from "../src/features/architecture/architecture-model.ts";

const roots = [{ root: "." }, { root: "architecture" }];
const views = [
  { id: "index", title: "System context" },
  { id: "containers", title: "Containers" },
];

test("keeps a selected root that still exists and otherwise selects the first", () => {
  assert.equal(pickModelRoot(roots, "architecture"), "architecture");
  assert.equal(pickModelRoot(roots, "removed"), ".");
  assert.equal(pickModelRoot([], "architecture"), null);
});

test("keeps a selected view that still exists and otherwise selects the first", () => {
  assert.equal(pickViewId(views, "containers"), "containers");
  assert.equal(pickViewId(views, "removed"), "index");
  assert.equal(pickViewId([], "index"), null);
});

test("formats diagnostics with and without a position", () => {
  assert.equal(
    diagnosticLocation({
      message: "bad",
      file: "architecture/model.c4",
      line: 12,
      column: 4,
    }),
    "architecture/model.c4:12:4",
  );
  assert.equal(
    diagnosticLocation({
      message: "bad",
      file: "architecture/model.c4",
      line: 12,
      column: null,
    }),
    "architecture/model.c4:12",
  );
  assert.equal(
    diagnosticLocation({
      message: "bad",
      file: null,
      line: null,
      column: null,
    }),
    "Model",
  );
});

test("labels the workspace root model", () => {
  assert.equal(rootLabel("."), "Workspace root");
  assert.equal(rootLabel("architecture"), "architecture");
});

test("rejects responses from an older request or another workspace", () => {
  assert.equal(isCurrentLoad(2, 2, "/workspace", "/workspace"), true);
  assert.equal(isCurrentLoad(1, 2, "/workspace", "/workspace"), false);
  assert.equal(isCurrentLoad(2, 2, "/other", "/workspace"), false);
});
