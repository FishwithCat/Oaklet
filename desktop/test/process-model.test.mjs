import assert from "node:assert/strict";
import test from "node:test";
import {
  formatDuration,
  processTitle,
  reasoningSummary,
  toolSummary,
  toolTitle,
} from "../src/features/chat/process-model.ts";

test("streaming reasoning previews the latest completed paragraph", () => {
  assert.equal(reasoningSummary("First line\nrest of paragraph", true), "First line");
  assert.equal(reasoningSummary("First\n\nSecond line\nmore", true), "Second line");
  assert.equal(reasoningSummary("One long unfinished line", true), "");
  assert.equal(reasoningSummary("**Bold** first line\nrest", false), "Bold first line");
});

test("labels MCP tools with their server and tool names", () => {
  assert.equal(toolTitle("mcp__github__search_issues"), "github · search_issues");
  assert.equal(toolTitle("read_file"), "Read file");
  assert.equal(toolTitle("read_image"), "Read image");
  assert.equal(toolTitle("grep"), "Search contents");
  assert.equal(toolTitle("glob"), "Find files");
  assert.equal(toolTitle("web_fetch"), "Fetch page");
  assert.equal(toolTitle("apply_patch"), "Apply patch");
  assert.equal(toolTitle("skill"), "Load skill");
});

test("formats command execution time after the summary", () => {
  assert.equal(formatDuration(400), "0.4s");
  assert.equal(formatDuration(12_000), "12s");
  assert.equal(formatDuration(65_000), "1m 5s");
  assert.equal(formatDuration(120_000), "2m");

  const command = {
    kind: "tool",
    runId: "run",
    id: "1",
    name: "run_command",
    status: "ok",
    summary: "pnpm test",
    durationMs: 12_000,
  };
  assert.equal(toolSummary(command), "pnpm test (12s)");
  assert.equal(toolSummary({ ...command, durationMs: undefined }), "pnpm test");
  assert.equal(
    toolSummary({ ...command, name: "read_file" }),
    "pnpm test",
  );
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
