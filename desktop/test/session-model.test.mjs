import assert from "node:assert/strict";
import test from "node:test";
import {
  applyApproval,
  applyCompaction,
  applyContext,
  applyDeltas,
  applyToolResult,
  applyToolStart,
  blankTab,
  finishDone,
  finishError,
  finishStopped,
  tabStatus,
  toChatTabInfo,
} from "../src/features/chat/session-model.ts";

const tab = (overrides = {}) => ({
  id: "tab-1",
  title: "Chat 1",
  running: true,
  completed: false,
  items: [],
  messages: [],
  activeRunId: null,
  runId: "run-1",
  savedSessionId: null,
  ...overrides,
});

test("blankTab takes its identity from the caller", () => {
  const created = blankTab("tab-7", 7);

  assert.equal(created.id, "tab-7");
  assert.equal(created.title, "Chat 7");
  assert.equal(created.running, false);
  assert.equal(created.completed, false);
  assert.deepEqual(created.items, []);
  assert.deepEqual(created.messages, []);
  assert.equal(created.context, null);
  assert.equal(created.activeRunId, null);
  assert.equal(created.runId, null);
  assert.equal(created.savedSessionId, null);
});

test("applyContext records the latest gauge without touching the transcript", () => {
  const updated = applyContext(tab(), { used: 12000, window: 262144 });

  assert.deepEqual(updated.context, { used: 12000, window: 262144 });
  assert.deepEqual(updated.items, []);
});

test("applyCompaction swaps the history and lowers the gauge by what it removed", () => {
  const messages = [{ role: "user", content: "<compacted-summary>..." }];
  const updated = applyCompaction(
    tab({ context: { used: 12000, window: 262144 } }),
    messages,
    9000,
    2000,
  );

  assert.deepEqual(updated.messages, messages);
  assert.deepEqual(updated.context, { used: 5000, window: 262144 });
});

test("applyCompaction never drives the gauge below zero", () => {
  const updated = applyCompaction(
    tab({ context: { used: 20, window: 262144 } }),
    [],
    100,
    5,
  );

  assert.equal(updated.context.used, 0);
});

test("tabStatus ranks a blocked tool above running, done, and idle", () => {
  assert.equal(tabStatus(tab()), "working");
  assert.equal(tabStatus(tab({ running: false, completed: true })), "done");
  assert.equal(tabStatus(tab({ running: false })), "idle");
  assert.equal(
    tabStatus(
      tab({
        items: [
          { kind: "tool", runId: "run-1", id: "tool-1", name: "run_command", status: "awaiting" },
        ],
      }),
    ),
    "block",
  );
  assert.deepEqual(toChatTabInfo(tab()), {
    id: "tab-1",
    title: "Chat 1",
    running: true,
    status: "working",
  });
});

test("applyDeltas merges adjacent assistant output and sets the active run", () => {
  const next = applyDeltas(tab(), "run-1", [
    { kind: "assistant", runId: "run-1", text: "Hel" },
    { kind: "assistant", runId: "run-1", text: "lo" },
  ]);

  assert.deepEqual(next.items, [
    { kind: "assistant", runId: "run-1", text: "Hello", streaming: true },
  ]);
  assert.equal(next.activeRunId, "run-1");
});

test("applyDeltas settles a stream at a kind or run boundary", () => {
  const first = applyDeltas(tab(), "run-1", [
    { kind: "reasoning", runId: "run-1", text: "first" },
    { kind: "reasoning", runId: "run-1", text: " thought" },
  ]);
  const second = applyDeltas(first, "run-2", [
    { kind: "reasoning", runId: "run-2", text: "second" },
  ]);

  assert.deepEqual(
    second.items.map((item) => [item.kind, item.text, item.streaming]),
    [
      ["reasoning", "first thought", false],
      ["reasoning", "second", true],
    ],
  );
  assert.equal(second.activeRunId, "run-2");
});

test("applyToolStart settles the current stream before appending the tool", () => {
  const streaming = applyDeltas(tab(), "run-1", [
    { kind: "assistant", runId: "run-1", text: "checking" },
  ]);
  const next = applyToolStart(
    streaming,
    { runId: "run-1", id: "tool-1", name: "read_file", summary: "a.ts" },
    1000,
  );

  assert.deepEqual(next.items, [
    { kind: "assistant", runId: "run-1", text: "checking", streaming: false },
    {
      kind: "tool",
      runId: "run-1",
      id: "tool-1",
      name: "read_file",
      status: "pending",
      summary: "a.ts",
      startedAt: 1000,
    },
  ]);
  assert.equal(next.activeRunId, "run-1");
});

test("applyToolResult records success or failure and the elapsed time", () => {
  const started = applyToolStart(
    tab(),
    { runId: "run-1", id: "tool-1", name: "run_command", summary: "ls" },
    1000,
  );
  const finished = applyToolResult(
    started,
    { id: "tool-1", ok: true, output: "ok" },
    1500,
  );

  assert.equal(finished.items[0].status, "ok");
  assert.equal(finished.items[0].output, "ok");
  assert.equal(finished.items[0].durationMs, 500);
  assert.equal(finished.items[0].startedAt, undefined);

  const failed = applyToolResult(
    started,
    { id: "tool-1", ok: false, output: "no" },
    1750,
  );
  assert.equal(failed.items[0].status, "error");
  assert.equal(failed.items[0].durationMs, 750);
});

test("applyApproval moves the matching tool into the awaiting state", () => {
  const started = applyToolStart(
    tab(),
    { runId: "run-1", id: "tool-1", name: "run_command", summary: "ls" },
    1000,
  );
  const awaiting = applyApproval(started, {
    id: "tool-1",
    requestId: "req-1",
    prompt: "Run?",
    detail: "ls -la",
  });

  assert.equal(awaiting.items[0].status, "awaiting");
  assert.equal(awaiting.items[0].requestId, "req-1");
  assert.equal(awaiting.items[0].approvalPrompt, "Run?");
  assert.equal(awaiting.items[0].approvalDetail, "ls -la");
});

test("finishDone settles items and stores messages", () => {
  const streaming = applyDeltas(tab(), "run-1", [
    { kind: "assistant", runId: "run-1", text: "done" },
  ]);
  const next = finishDone(streaming, [{ role: "assistant", content: "done" }]);

  assert.equal(next.running, false);
  assert.equal(next.completed, true);
  assert.equal(next.activeRunId, null);
  assert.equal(next.runId, null);
  assert.deepEqual(next.messages, [{ role: "assistant", content: "done" }]);
  assert.equal(next.items[0].streaming, false);
});

test("finishError and finishStopped keep their distinct notices", () => {
  const failed = finishError(tab(), "network down");
  assert.equal(failed.running, false);
  assert.equal(failed.completed, false);
  assert.deepEqual(failed.items.at(-1), {
    kind: "notice",
    tone: "error",
    text: "network down",
  });

  const stopped = finishStopped(tab());
  assert.equal(stopped.running, false);
  assert.equal(stopped.completed, false);
  assert.deepEqual(stopped.items.at(-1), {
    kind: "notice",
    tone: "muted",
    text: "Stopped.",
  });
});
