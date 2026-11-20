import assert from "node:assert/strict";
import test from "node:test";
import {
  findMatches,
  locateOffset,
  stepMatch,
} from "../src/features/chat/find-model.ts";

test("findMatches is case-insensitive and returns every occurrence", () => {
  assert.deepEqual(findMatches("Favicon and favicon.svg", "favicon"), [
    { start: 0, end: 7 },
    { start: 12, end: 19 },
  ]);
  assert.deepEqual(findMatches("aaaa", "aa"), [
    { start: 0, end: 2 },
    { start: 2, end: 4 },
  ]);
});

test("findMatches treats the query as literal text", () => {
  assert.deepEqual(findMatches("a.b (c)", "."), [{ start: 1, end: 2 }]);
  assert.deepEqual(findMatches("a.b (c)", "(c)"), [{ start: 4, end: 7 }]);
  assert.deepEqual(findMatches("no dots", "."), []);
});

test("findMatches returns nothing for an empty query", () => {
  assert.deepEqual(findMatches("anything", ""), []);
});

test("stepMatch wraps in both directions and stays at zero without matches", () => {
  assert.equal(stepMatch(0, 3, 1), 1);
  assert.equal(stepMatch(2, 3, 1), 0);
  assert.equal(stepMatch(0, 3, -1), 2);
  assert.equal(stepMatch(0, 0, 1), 0);
});

test("locateOffset maps concatenated offsets back onto text runs", () => {
  const runs = { starts: [0, 2, 7], lengths: [2, 5, 3] };
  assert.deepEqual(locateOffset(runs, 0), { index: 0, offset: 0 });
  assert.deepEqual(locateOffset(runs, 2), { index: 1, offset: 0 });
  assert.deepEqual(locateOffset(runs, 4), { index: 1, offset: 2 });
  assert.deepEqual(locateOffset(runs, 10), { index: 2, offset: 3 });
  assert.deepEqual(locateOffset({ starts: [], lengths: [] }, 0), null);
});
