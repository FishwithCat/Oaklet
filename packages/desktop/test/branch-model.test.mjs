import assert from "node:assert/strict";
import test from "node:test";
import { filterBranches } from "../src/features/source-control/git-model.ts";

const branches = ["main", "feature/branch-menu", "bugfix/header"];

test("branch search is case-insensitive and trims the query", () => {
  assert.deepEqual(filterBranches(branches, "FEATURE"), [
    "feature/branch-menu",
  ]);
  assert.deepEqual(filterBranches(branches, "  menu  "), [
    "feature/branch-menu",
  ]);
});

test("an empty query keeps every branch in order", () => {
  assert.deepEqual(filterBranches(branches, ""), branches);
  assert.deepEqual(filterBranches(branches, "   "), branches);
});

test("a query without matches yields an empty list", () => {
  assert.deepEqual(filterBranches(branches, "nope"), []);
});
