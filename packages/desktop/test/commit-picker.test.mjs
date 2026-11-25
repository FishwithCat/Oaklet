import assert from "node:assert/strict";
import test from "node:test";
import { filterCommits } from "../src/features/source-control/git-model.ts";

const recent = [
  {
    hash: "abcd1234ef56789",
    shortHash: "abcd123",
    subject: "Fix theme preview",
    body: "Keeps the dark palette readable.",
    author: "Lee",
    email: "lee@example.com",
    refs: ["main", "origin/main"],
  },
  {
    hash: "ef7890abc012345",
    shortHash: "ef7890a",
    subject: "更新终端颜色",
    body: "",
    author: "Ren",
    email: "ren@example.com",
    refs: ["terminal"],
  },
];

test("commit search matches hashes, authors, messages, and refs", () => {
  assert.deepEqual(filterCommits(recent, "  ABcd123  "), [recent[0]]);
  assert.deepEqual(filterCommits(recent, recent[1].hash), [recent[1]]);
  assert.deepEqual(filterCommits(recent, "dark ORIGIN/MAIN"), [recent[0]]);
  assert.deepEqual(filterCommits(recent, "颜色 ren@example.com"), [recent[1]]);
  assert.deepEqual(filterCommits(recent, "no match"), []);
});

test("commit search preserves history order for an empty or shared query", () => {
  assert.deepEqual(filterCommits(recent, ""), recent);
  assert.deepEqual(filterCommits(recent, "example.com"), recent);
});
