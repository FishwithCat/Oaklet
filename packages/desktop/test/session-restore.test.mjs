import assert from "node:assert/strict";
import test from "node:test";
import {
  lastSessionKey,
  pickStartupSession,
  readLastSessionId,
  rememberLastSession,
} from "../src/features/chat/session-restore.ts";

const summary = (id, updatedAt) => ({
  id,
  title: id,
  updatedAt,
  turns: 1,
  running: false,
  blocked: false,
  compacting: false,
});

function memoryStore() {
  const values = new Map();
  return {
    values,
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => {
      values.set(key, value);
    },
  };
}

test("the remembered session wins even when another was updated later", () => {
  const list = [summary("newest", 30), summary("remembered", 20)];
  assert.equal(pickStartupSession(list, "remembered").id, "remembered");
});

test("a missing remembered id falls back to the newest history row", () => {
  const list = [summary("newest", 30), summary("older", 10)];
  assert.equal(pickStartupSession(list, "deleted").id, "newest");
});

test("no preference falls back to the newest history row", () => {
  const list = [summary("newest", 30), summary("older", 10)];
  assert.equal(pickStartupSession(list, null).id, "newest");
});

test("an empty history has no startup target", () => {
  assert.equal(pickStartupSession([], "remembered"), null);
});

test("preferences are kept per canonical workspace path", () => {
  const store = memoryStore();
  rememberLastSession("/work/alpha", "alpha-session", store);
  rememberLastSession("/work/beta", "beta-session", store);

  assert.equal(lastSessionKey("/work/alpha"), "oaklet:last-session:/work/alpha");
  assert.equal(readLastSessionId("/work/alpha", store), "alpha-session");
  assert.equal(readLastSessionId("/work/beta", store), "beta-session");
  assert.equal(readLastSessionId("/work/gamma", store), null);
});

test("storage failures leave chat working without a preference", () => {
  const store = {
    getItem() {
      throw new Error("storage blocked");
    },
    setItem() {
      throw new Error("storage blocked");
    },
  };
  assert.equal(readLastSessionId("/work/alpha", store), null);
  assert.doesNotThrow(() =>
    rememberLastSession("/work/alpha", "session", store),
  );
  assert.equal(readLastSessionId("/work/alpha", null), null);
  assert.doesNotThrow(() =>
    rememberLastSession("/work/alpha", "session", null),
  );
});
