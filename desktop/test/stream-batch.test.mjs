import assert from "node:assert/strict";
import test from "node:test";
import { createStreamBatch } from "../src/features/chat/stream-batch.ts";

test("stream batching merges adjacent deltas, preserves order and flushes at boundaries", () => {
  const frames = new Map();
  let id = 0;
  globalThis.requestAnimationFrame = (callback) => { frames.set(++id, callback); return id; };
  globalThis.cancelAnimationFrame = (frame) => frames.delete(frame);
  try {
    const updates = [];
    const batch = createStreamBatch((deltas) => updates.push(deltas));
    const push = (kind, text, runId = "run") => batch.push({ kind, text, runId });
    push("reasoning", "a");
    push("reasoning", "b");
    push("assistant", "c");
    assert.equal(frames.size, 1);
    assert.equal(updates.length, 0);
    [...frames.values()][0]();
    assert.deepEqual(updates[0], [
      { kind: "reasoning", text: "ab", runId: "run" },
      { kind: "assistant", text: "c", runId: "run" },
    ]);
    push("assistant", "tail");
    push("assistant", "next", "other");
    batch.flush();
    assert.equal(updates[1].length, 2);
    assert.equal(frames.size, 0);
    batch.flush();
    assert.equal(updates.length, 2);
    push("assistant", "discard on workspace switch");
    batch.clear();
    assert.equal(frames.size, 0);
    batch.flush();
    assert.equal(updates.length, 2);
  } finally {
    delete globalThis.requestAnimationFrame;
    delete globalThis.cancelAnimationFrame;
  }
});
