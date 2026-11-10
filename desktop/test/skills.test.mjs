import assert from "node:assert/strict";
import test from "node:test";
import { sourceLabel } from "../src/skills.ts";

test("labels skill sources in the skills popup", () => {
  assert.equal(sourceLabel("project-oaklet"), "Project · .oaklet");
  assert.equal(sourceLabel("project-agents"), "Project · .agents");
  assert.equal(sourceLabel("user-oaklet"), "User · .oaklet");
  assert.equal(sourceLabel("user-agents"), "User · .agents");
});

test("keeps an unknown source visible", () => {
  assert.equal(sourceLabel("custom"), "custom");
});
