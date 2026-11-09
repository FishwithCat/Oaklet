import assert from "node:assert/strict";
import test from "node:test";
import {
  loadSessions,
  mergeSessions,
  saveSession,
  sessionTitle,
} from "../src/session-history.ts";

const snapshot = (id, updatedAt) => ({
  id,
  title: id,
  updatedAt,
  messages: [],
  items: [{ kind: "user", text: id }],
});

test("session title uses the first user message on one line", () => {
  const items = [
    { kind: "assistant", text: "hello", streaming: false },
    { kind: "user", text: "  Fix\n the   parser  " },
    { kind: "user", text: "later" },
  ];
  assert.equal(sessionTitle(items), "Fix the parser");
  assert.equal(sessionTitle([]), "");
  assert.equal(sessionTitle([{ kind: "user", text: "x".repeat(80) }]).length, 60);
});

test("mergeSessions dedupes by id, sorts newest first, and caps the list", () => {
  const merged = mergeSessions(
    snapshot("b", 3),
    [snapshot("a", 1), snapshot("b", 2)],
    2,
  );

  assert.deepEqual(
    merged.map((session) => session.id),
    ["b", "a"],
  );

  const capped = mergeSessions(snapshot("c", 3), [snapshot("a", 1)], 1);
  assert.deepEqual(capped.map((session) => session.id), ["c"]);
});

test("saveSession drops the oldest sessions when storage is full", () => {
  const store = new Map();
  const workspace = "/workspace";
  const key = `oaklet.history.v1:${workspace}`;
  store.set(key, JSON.stringify([snapshot("a", 1), snapshot("b", 2)]));

  const newest = {
    ...snapshot("c", 3),
    messages: ["x".repeat(200)],
  };
  const limit = JSON.stringify([newest]).length;

  globalThis.localStorage = {
    getItem: (name) => store.get(name) ?? null,
    setItem: (name, value) => {
      if (value.length > limit) throw new Error("quota");
      store.set(name, value);
    },
    removeItem: (name) => store.delete(name),
  };

  try {
    const next = saveSession(workspace, newest);
    assert.deepEqual(next.map((session) => session.id), ["c"]);
    assert.deepEqual(
      JSON.parse(store.get(key)).map((session) => session.id),
      ["c"],
    );
  } finally {
    delete globalThis.localStorage;
  }
});

test("sessions are scoped to their workspace", () => {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (name) => store.get(name) ?? null,
    setItem: (name, value) => store.set(name, value),
    removeItem: (name) => store.delete(name),
  };

  try {
    saveSession("/one", snapshot("one", 1));
    saveSession("/two", snapshot("two", 2));

    assert.deepEqual(
      loadSessions("/one").map((session) => session.id),
      ["one"],
    );
    assert.deepEqual(
      loadSessions("/two").map((session) => session.id),
      ["two"],
    );
  } finally {
    delete globalThis.localStorage;
  }
});

test("unscoped history moves to the first workspace that loads it", () => {
  const store = new Map([["oaklet.history.v1", JSON.stringify([snapshot("old", 1)])]]);
  globalThis.localStorage = {
    getItem: (name) => store.get(name) ?? null,
    setItem: (name, value) => store.set(name, value),
    removeItem: (name) => store.delete(name),
  };

  try {
    assert.deepEqual(
      loadSessions("/workspace").map((session) => session.id),
      ["old"],
    );
    assert.equal(store.has("oaklet.history.v1"), false);
  } finally {
    delete globalThis.localStorage;
  }
});
