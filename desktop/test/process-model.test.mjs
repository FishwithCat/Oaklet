import assert from "node:assert/strict";
import test from "node:test";
import {
  processTitle,
  reasoningSummary,
  toolTitle,
} from "../src/process-model.ts";

test("streaming reasoning previews the latest completed paragraph", () => {
  assert.equal(reasoningSummary("First line\nrest of paragraph", true), "First line");
  assert.equal(reasoningSummary("First\n\nSecond line\nmore", true), "Second line");
  assert.equal(reasoningSummary("One long unfinished line", true), "");
  assert.equal(reasoningSummary("**Bold** first line\nrest", false), "Bold first line");
});

test("labels MCP tools with their server and tool names", () => {
  assert.equal(toolTitle("mcp__github__search_issues"), "github · search_issues");
  assert.equal(toolTitle("read_file"), "Read file");
});

test("process title ranks tool categories after settlement", () => {
  const items = [
    { kind: "tool", runId: "run", id: "1", name: "read_file", status: "ok" },
    { kind: "tool", runId: "run", id: "2", name: "read_file", status: "ok" },
    { kind: "tool", runId: "run", id: "3", name: "run_command", status: "ok" },
  ];
  assert.equal(processTitle(items, false), "Read files and ran commands");
  assert.equal(
    processTitle(
      [...items, { kind: "tool", runId: "run", id: "4", name: "list_dir", status: "running" }],
      true,
    ),
    "Reading files",
  );
  assert.equal(
    processTitle(
      [{ kind: "reasoning", runId: "run", text: "thinking", streaming: false }],
      false,
    ),
    "Analysis completed",
  );
});
