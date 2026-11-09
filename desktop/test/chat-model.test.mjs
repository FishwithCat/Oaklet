import assert from "node:assert/strict";
import test from "node:test";
import { settleStreaming, toBlocks } from "../src/chat-model.ts";

const reasoning = (text = "thinking") => ({
  kind: "reasoning",
  runId: "run",
  text,
  streaming: false,
});

const tool = (id, status = "ok") => ({
  kind: "tool",
  runId: "run",
  id,
  name: "read_file",
  status,
});

test("consecutive reasoning and tool items form one trailing process group", () => {
  const items = [reasoning(), tool("1"), reasoning("more")];
  const blocks = toBlocks(items);

  assert.deepEqual(blocks, [{ kind: "process", start: 0, items }]);
});

test("messages separate process groups while preserving original indices", () => {
  const items = [
    { kind: "user", text: "hi" },
    reasoning(),
    tool("1"),
    { kind: "assistant", text: "done", streaming: false },
    reasoning("again"),
    tool("2", "error"),
  ];
  const blocks = toBlocks(items);

  assert.deepEqual(blocks, [
    { kind: "single", index: 0, item: items[0] },
    { kind: "process", start: 1, items: [items[1], items[2]] },
    { kind: "single", index: 3, item: items[3] },
    { kind: "process", start: 4, items: [items[4], items[5]] },
  ]);
});

test("settleStreaming clears only streaming assistant and reasoning items", () => {
  const settledAssistant = { kind: "assistant", text: "old", streaming: false };
  const streamingAssistant = { kind: "assistant", text: "new", streaming: true };
  const settledReasoning = reasoning("settled");
  const streamingReasoning = { ...reasoning("streaming"), streaming: true };
  const user = { kind: "user", text: "hi" };
  const pendingTool = tool("1", "pending");
  const items = [
    settledAssistant,
    streamingAssistant,
    settledReasoning,
    streamingReasoning,
    user,
    pendingTool,
  ];

  const next = settleStreaming(items);

  assert.deepEqual(next, [
    settledAssistant,
    { ...streamingAssistant, streaming: false },
    settledReasoning,
    { ...streamingReasoning, streaming: false },
    user,
    pendingTool,
  ]);
  assert.equal(next[0], settledAssistant);
  assert.equal(next[2], settledReasoning);
  assert.equal(next[4], user);
  assert.equal(next[5], pendingTool);
});
