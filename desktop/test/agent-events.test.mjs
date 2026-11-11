import assert from "node:assert/strict";
import test from "node:test";
import { subscribeAgentEvents } from "../src/features/chat/agent-events.ts";

const handlers = () => ({
  onDelta: () => {},
  onReasoning: () => {},
  onToolStart: () => {},
  onToolResult: () => {},
  onApproval: () => {},
  onApprovalResolved: () => {},
  onDone: () => {},
  onContext: () => {},
  onError: () => {},
  onStopped: () => {},
});

test("registers every agent event and releases all listeners", async () => {
  const registered = new Map();
  const released = [];
  const register = async (event, handler) => {
    registered.set(event, handler);
    return () => released.push(event);
  };

  const release = await subscribeAgentEvents(handlers(), register);

  assert.deepEqual([...registered.keys()], [
    "agent:delta",
    "agent:reasoning",
    "agent:tool_start",
    "agent:tool_result",
    "agent:approval",
    "agent:approval_resolved",
    "agent:done",
    "agent:context",
    "agent:error",
    "agent:stopped",
  ]);

  release();
  assert.deepEqual(released, [...registered.keys()]);
});

test("dispatches each payload to its handler", async () => {
  const registered = new Map();
  const seen = [];
  const register = async (event, handler) => {
    registered.set(event, handler);
    return () => {};
  };
  const received = handlers();
  received.onDelta = (payload) => seen.push(["delta", payload]);
  received.onDone = (payload) => seen.push(["done", payload]);

  await subscribeAgentEvents(received, register);
  registered.get("agent:delta")({
    sessionId: "session-1",
    runId: "run-1",
    text: "hi",
  });
  registered.get("agent:done")({
    sessionId: "session-1",
    runId: "run-1",
    session: null,
  });

  assert.deepEqual(seen, [
    ["delta", { sessionId: "session-1", runId: "run-1", text: "hi" }],
    ["done", { sessionId: "session-1", runId: "run-1", session: null }],
  ]);
});

test("a failed registration releases the listeners that already succeeded", async () => {
  const released = [];
  let calls = 0;
  const register = async (event) => {
    calls += 1;
    if (calls === 3) throw new Error("listen failed");
    return () => released.push(event);
  };

  await assert.rejects(
    subscribeAgentEvents(handlers(), register),
    /listen failed/,
  );
  assert.deepEqual(released, ["agent:delta", "agent:reasoning"]);
});
