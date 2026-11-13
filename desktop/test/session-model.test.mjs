import assert from "node:assert/strict";
import test from "node:test";
import {
  appendNotice,
  applyApproval,
  applyApprovalResolved,
  applyContext,
  applyDeltas,
  applyToolResult,
  applyToolStart,
  mergeWindow,
  prependWindow,
  settleRun,
  tabStatus,
  toChatTabInfo,
  withItems,
} from "../src/features/chat/session-model.ts";

const session = (overrides = {}) => ({
  id: "session-1",
  title: "",
  updatedAt: 1,
  turns: 0,
  items: [],
  firstSeq: null,
  hasMore: false,
  context: null,
  running: true,
  blocked: false,
  compacting: false,
  activeRunId: null,
  ...overrides,
});

const delta = (kind, text, runId = "run-1") => ({
  kind,
  sessionId: "session-1",
  runId,
  text,
});

test("withItems keeps the blocked flag in sync with awaiting tools", () => {
  const items = [
    { kind: "tool", runId: "run-1", id: "t1", name: "run_command", status: "awaiting" },
  ];
  assert.equal(withItems(session(), items).blocked, true);
  assert.equal(
    withItems(session(), [
      { kind: "tool", runId: "run-1", id: "t1", name: "run_command", status: "ok" },
    ]).blocked,
    false,
  );
});

test("applyContext records the latest gauge without touching the transcript", () => {
  const updated = applyContext(session(), { used: 12000, window: 262144 });

  assert.deepEqual(updated.context, { used: 12000, window: 262144 });
  assert.deepEqual(updated.items, []);
});

test("tabStatus ranks a blocked tool above running, done, and idle", () => {
  assert.equal(tabStatus(session()), "working");
  assert.equal(tabStatus(session({ running: false, turns: 1 })), "done");
  assert.equal(tabStatus(session({ running: false })), "idle");
  assert.equal(
    tabStatus(
      session({
        items: [
          { kind: "tool", runId: "run-1", id: "tool-1", name: "run_command", status: "awaiting" },
        ],
      }),
    ),
    "block",
  );
  assert.deepEqual(toChatTabInfo(session({ running: false }), 3), {
    id: "session-1",
    title: "Chat 3",
    running: false,
    status: "idle",
  });
});

test("applyDeltas merges adjacent assistant output and sets the active run", () => {
  const next = applyDeltas(session(), "run-1", [
    delta("assistant", "Hel"),
    delta("assistant", "lo"),
  ]);

  assert.deepEqual(next.items, [
    { kind: "assistant", runId: "run-1", text: "Hello", streaming: true },
  ]);
  assert.equal(next.activeRunId, "run-1");
  assert.equal(next.running, true);
});

test("applyDeltas settles a stream at a kind or run boundary", () => {
  const first = applyDeltas(session(), "run-1", [
    delta("reasoning", "first"),
    delta("reasoning", " thought"),
  ]);
  const second = applyDeltas(first, "run-2", [
    delta("reasoning", "second", "run-2"),
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
  const streaming = applyDeltas(session(), "run-1", [delta("assistant", "checking")]);
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
    session(),
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

test("tool diffs survive the pending row and are replaced by the applied ones", () => {
  const preview = [
    { path: "a.txt", oldText: null, newText: "one" },
  ];
  const applied = [
    { path: "a.txt", oldText: null, newText: "one\ntwo" },
  ];
  const started = applyToolStart(
    session(),
    {
      runId: "run-1",
      id: "tool-1",
      name: "write_file",
      summary: "a.txt",
      diffs: preview,
    },
    1000,
  );
  assert.deepEqual(started.items[0].diffs, preview);

  const finished = applyToolResult(
    started,
    { id: "tool-1", ok: true, output: "ok", diffs: applied },
    1500,
  );
  assert.deepEqual(finished.items[0].diffs, applied);

  const failed = applyToolResult(
    started,
    { id: "tool-1", ok: false, output: "no", diffs: [] },
    1500,
  );
  assert.equal(failed.items[0].diffs, undefined);
});

test("applyApproval moves the matching tool into the awaiting state", () => {
  const started = applyToolStart(
    session(),
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
  assert.equal(awaiting.blocked, true);
});

test("applyApprovalResolved clears the card and starts the clock", () => {
  const started = applyToolStart(
    session(),
    { runId: "run-1", id: "tool-1", name: "run_command", summary: "ls" },
    1000,
  );
  const awaiting = applyApproval(started, {
    id: "tool-1",
    requestId: "req-1",
    prompt: "Run?",
    detail: "ls -la",
  });
  const allowed = applyApprovalResolved(awaiting, { id: "tool-1", allow: true }, 2000);

  assert.equal(allowed.items[0].status, "running");
  assert.equal(allowed.items[0].startedAt, 2000);
  assert.equal(allowed.items[0].requestId, undefined);
  assert.equal(allowed.blocked, false);

  const denied = applyApprovalResolved(awaiting, { id: "tool-1", allow: false }, 2000);
  assert.equal(denied.items[0].status, "running");
  assert.equal(denied.items[0].startedAt, undefined);
});

test("settleRun settles streaming and unfinished tools when the session is lost", () => {
  const streaming = applyDeltas(
    applyToolStart(
      session(),
      {
        runId: "run-1",
        id: "tool-1",
        name: "write_file",
        summary: "a.txt",
        diffs: [{ path: "a.txt", oldText: null, newText: "one" }],
      },
      1000,
    ),
    "run-1",
    [delta("assistant", "checking")],
  );
  const settled = settleRun(streaming, { tone: "error", text: "save failed" });

  assert.equal(settled.running, false);
  assert.equal(settled.activeRunId, null);
  assert.equal(settled.blocked, false);
  assert.equal(settled.items[0].status, "error");
  assert.equal(settled.items[0].diffs, undefined);
  assert.equal(settled.items[1].streaming, false);
  assert.deepEqual(settled.items.at(-1), {
    kind: "notice",
    tone: "error",
    text: "save failed",
  });
  assert.equal(appendNotice(settled, "muted", "later").items.length, 4);
});

test("mergeWindow replaces the suffix and keeps older loaded pages", () => {
  const current = session({
    firstSeq: 1,
    hasMore: true,
    items: [
      { kind: "user", text: "old", seq: 1 },
      { kind: "assistant", text: "old answer", streaming: false, seq: 2 },
      { kind: "user", text: "new", seq: 3 },
      { kind: "assistant", text: "draft", streaming: true },
    ],
  });
  const incoming = session({
    firstSeq: 3,
    hasMore: true,
    items: [
      { kind: "user", text: "new", seq: 3 },
      { kind: "assistant", text: "final", streaming: false, seq: 4 },
    ],
  });

  const merged = mergeWindow(current, incoming);
  assert.deepEqual(
    merged.items.map((item) => item.text),
    ["old", "old answer", "new", "final"],
  );
  assert.equal(merged.firstSeq, 1);
  assert.equal(merged.hasMore, true);
});

test("mergeWindow takes the incoming cursor when it replaces the whole window", () => {
  const merged = mergeWindow(
    session({
      firstSeq: 5,
      hasMore: true,
      items: [{ kind: "user", text: "new", seq: 5 }],
    }),
    session({
      firstSeq: 1,
      hasMore: false,
      items: [{ kind: "user", text: "start", seq: 1 }],
    }),
  );

  assert.deepEqual(
    merged.items.map((item) => item.text),
    ["start"],
  );
  assert.equal(merged.firstSeq, 1);
  assert.equal(merged.hasMore, false);
});

test("prependWindow only moves the cursor farther back", () => {
  const current = session({
    firstSeq: 5,
    hasMore: true,
    items: [{ kind: "user", text: "new", seq: 5 }],
  });
  const older = session({
    firstSeq: 3,
    hasMore: false,
    items: [{ kind: "user", text: "old", seq: 3 }],
  });
  const merged = prependWindow(current, older);

  assert.deepEqual(
    merged.items.map((item) => item.text),
    ["old", "new"],
  );
  assert.equal(merged.firstSeq, 3);
  assert.equal(merged.hasMore, false);
  assert.equal(prependWindow(merged, older), merged);

  const overlapping = prependWindow(current, session({
    firstSeq: 3,
    hasMore: true,
    items: [
      { kind: "user", text: "old", seq: 3 },
      { kind: "user", text: "new", seq: 5 },
    ],
  }));
  assert.deepEqual(
    overlapping.items.map((item) => item.text),
    ["old", "new"],
  );
});
