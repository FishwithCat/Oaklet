import assert from "node:assert/strict";
import test from "node:test";
import {
  collectLegacyEntries,
  importLegacySessions,
  parseLegacySessions,
} from "../src/features/chat/session-import.ts";

class FakeStorage {
  #items = new Map();

  get length() {
    return this.#items.size;
  }

  key(index) {
    return [...this.#items.keys()][index] ?? null;
  }

  getItem(key) {
    return this.#items.get(key) ?? null;
  }

  setItem(key, value) {
    this.#items.set(key, value);
  }

  removeItem(key) {
    this.#items.delete(key);
  }
}

const snapshot = (id, updatedAt = 1) => ({
  id,
  title: id,
  updatedAt,
  messages: [{ role: "user", content: id }],
  items: [{ kind: "user", text: id }],
});

const KEY = "oaklet.history.v1";

test("legacy history is grouped by workspace and the old key uses the current one", () => {
  const storage = new FakeStorage();
  storage.setItem(`${KEY}:/tmp/a`, JSON.stringify([snapshot("a1")]));
  storage.setItem(`${KEY}:/tmp/b`, JSON.stringify([snapshot("b1")]));
  storage.setItem(KEY, JSON.stringify([snapshot("legacy")]));
  storage.setItem("unrelated", JSON.stringify([snapshot("nope")]));

  const { keys, entries } = collectLegacyEntries(storage, "/tmp/a");
  const byWorkspace = Object.fromEntries(
    entries.map((entry) => [entry.workspace, entry.sessions.map((s) => s.id)]),
  );

  assert.deepEqual(keys.sort(), [`${KEY}:/tmp/a`, `${KEY}:/tmp/b`, KEY].sort());
  assert.deepEqual(byWorkspace["/tmp/a"], ["a1", "legacy"]);
  assert.deepEqual(byWorkspace["/tmp/b"], ["b1"]);
  assert.deepEqual(parseLegacySessions(storage.getItem(`${KEY}:/tmp/b`)), [
    snapshot("b1"),
  ]);
});

test("invalid snapshots are skipped and malformed JSON is ignored", () => {
  assert.deepEqual(parseLegacySessions("not json"), []);
  assert.deepEqual(
    parseLegacySessions(
      JSON.stringify([
        { id: "bad", title: "Bad", updatedAt: 1 },
        snapshot("good"),
      ]),
    ),
    [snapshot("good")],
  );
});

test("import sends one entry per workspace and removes keys after success", async () => {
  const storage = new FakeStorage();
  storage.setItem(`${KEY}:/tmp/a`, JSON.stringify([snapshot("a1")]));
  storage.setItem(`${KEY}:/tmp/b`, JSON.stringify([snapshot("b1")]));
  storage.setItem(KEY, JSON.stringify([snapshot("legacy")]));
  const seen = [];

  const imported = await importLegacySessions(storage, "/tmp/a", async (entries) => {
    seen.push(entries);
    return 3;
  });

  assert.equal(imported, 3);
  assert.equal(seen.length, 1);
  assert.deepEqual(
    seen[0].map((entry) => [entry.workspace, entry.sessions.length]),
    [
      ["/tmp/a", 2],
      ["/tmp/b", 1],
    ],
  );
  assert.equal(storage.length, 0);
});

test("a failed import keeps every key for the next launch", async () => {
  const storage = new FakeStorage();
  storage.setItem(`${KEY}:/tmp/a`, JSON.stringify([snapshot("a1")]));

  await assert.rejects(
    importLegacySessions(storage, "/tmp/a", async () => {
      throw new Error("backend down");
    }),
    /backend down/,
  );
  assert.equal(storage.getItem(`${KEY}:/tmp/a`) !== null, true);
});

test("keys with no usable sessions are cleaned up without calling the backend", async () => {
  const storage = new FakeStorage();
  storage.setItem(`${KEY}:/tmp/a`, "not json");
  let calls = 0;

  const imported = await importLegacySessions(storage, "/tmp/a", async () => {
    calls += 1;
    return 0;
  });

  assert.equal(imported, 0);
  assert.equal(calls, 0);
  assert.equal(storage.length, 0);
});
