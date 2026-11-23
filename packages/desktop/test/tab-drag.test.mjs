import assert from "node:assert/strict";
import test from "node:test";
import { moveItem } from "../src/shared/useTabDrag.ts";

test("moves a tab to its drop index", () => {
  const tabs = ["a", "b", "c", "d"];
  assert.deepEqual(moveItem(tabs, 0, 2), ["b", "c", "a", "d"]);
  assert.deepEqual(moveItem(tabs, 3, 0), ["d", "a", "b", "c"]);
});

test("keeps the same array and leaves the input untouched on a no-op", () => {
  const tabs = ["a", "b", "c"];
  assert.equal(moveItem(tabs, 1, 1), tabs);
  assert.equal(moveItem(tabs, -1, 0), tabs);
  assert.equal(moveItem(tabs, 0, 3), tabs);
  assert.deepEqual(tabs, ["a", "b", "c"]);
});
