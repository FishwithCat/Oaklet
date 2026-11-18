import assert from "node:assert/strict";
import test from "node:test";
import {
  formatDuration,
  processTitle,
  reasoningSummary,
  processDuration,
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

test("formats command execution time independently of the summary", () => {
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
  assert.equal(processDuration(command, 99_000), "12s");
  assert.equal(processDuration({ ...command, status: "error" }, 99_000), "12s");
  assert.equal(processDuration({ ...command, summary: "" }, 99_000), "12s");
  assert.equal(processDuration({ ...command, durationMs: 0 }, 99_000), "0.0s");
  assert.equal(processDuration({ ...command, durationMs: undefined }, 99_000), undefined);
  assert.equal(
    processDuration({ ...command, name: "read_file" }, 99_000),
    undefined,
  );
});

test("updates running commands without counting approval waits or inventing old timings", () => {
  const command = {
    kind: "tool", runId: "run", id: "1", name: "run_command",
    status: "pending", startedAt: 1000,
  };
  assert.equal(processDuration(command, 1400), "0.4s");
  assert.equal(processDuration(command, 13_000), "12s");
  assert.equal(processDuration({ ...command, status: "running" }, 13_000), "12s");
  assert.equal(processDuration({ ...command, status: "awaiting" }, 13_000), undefined);
  assert.equal(processDuration({ ...command, status: "error" }, 13_000), undefined);
  assert.equal(processDuration({ ...command, startedAt: undefined }, 13_000), undefined);
  assert.equal(processDuration(command, 900), "0.0s");
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

test("shows live and settled thinking time without inventing historical timings", () => {
  const thinking = {
    kind: "reasoning", runId: "run", text: "thinking", streaming: true, startedAt: 1000,
  };
  assert.equal(processDuration(thinking, 1400), "0.4s");
  assert.equal(processDuration(thinking, 66_000), "1m 5s");
  assert.equal(processDuration({ ...thinking, streaming: false, durationMs: 2500 }, 99_000), "2.5s");
  assert.equal(processDuration({ ...thinking, streaming: false }, 99_000), undefined);
  assert.equal(processDuration({ ...thinking, startedAt: undefined }, 99_000), undefined);
});
