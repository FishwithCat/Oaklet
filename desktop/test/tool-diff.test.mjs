import assert from "node:assert/strict";
import test from "node:test";
import {
  buildDiffRows,
  diffTotals,
} from "../src/features/chat/tool-diff.ts";

test("a created file is one path header and additions", () => {
  const rows = buildDiffRows([
    { path: "src/a.ts", oldText: null, newText: "one\ntwo\n" },
  ]);

  assert.deepEqual(rows, [
    { kind: "path", path: "src/a.ts", added: 2, removed: 0 },
    { kind: "add", text: "one" },
    { kind: "add", text: "two" },
  ]);
  assert.deepEqual(diffTotals([{ path: "src/a.ts", oldText: null, newText: "one\ntwo\n" }]), {
    added: 2,
    removed: 0,
  });
});

test("an edit keeps shared context around the changed line", () => {
  const rows = buildDiffRows([
    {
      path: "notes.txt",
      oldText: "alpha\nbeta\ngamma",
      newText: "alpha\nBETA\ngamma",
    },
  ]);

  assert.deepEqual(rows, [
    { kind: "path", path: "notes.txt", added: 1, removed: 1 },
    { kind: "context", text: "alpha" },
    { kind: "del", text: "beta" },
    { kind: "add", text: "BETA" },
    { kind: "context", text: "gamma" },
  ]);
});

test("separate hunks of one file share a path header and gain a gap", () => {
  const diffs = [
    {
      path: "a.txt",
      oldText: "a\nb\nc",
      newText: "a\nB\nc",
    },
    {
      path: "a.txt",
      oldText: "x\ny\nz",
      newText: "x\nY\nz",
    },
  ];
  const rows = buildDiffRows(diffs);

  assert.deepEqual(rows[0], {
    kind: "path",
    path: "a.txt",
    added: 2,
    removed: 2,
  });
  assert.equal(
    rows.filter((row) => row.kind === "gap").length,
    1,
  );
  assert.deepEqual(diffTotals(diffs), { added: 2, removed: 2 });
});

test("a deletion has no added lines and tolerates CRLF", () => {
  const rows = buildDiffRows([
    { path: "old.txt", oldText: "one\r\ntwo\r\n", newText: "" },
  ]);

  assert.deepEqual(rows, [
    { kind: "path", path: "old.txt", added: 0, removed: 2 },
    { kind: "del", text: "one" },
    { kind: "del", text: "two" },
  ]);
});

test("an empty diff list renders nothing", () => {
  assert.deepEqual(buildDiffRows([]), []);
  assert.deepEqual(diffTotals([]), { added: 0, removed: 0 });
});
