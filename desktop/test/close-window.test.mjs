import assert from "node:assert/strict";
import test from "node:test";
import { createWindowCloseHandler } from "../src/app/close-window.ts";

function setup(overrides = {}) {
  const seen = [];
  const close = createWindowCloseHandler({
    isBusy: () => false,
    confirmStop: async () => { seen.push("confirm"); return true; },
    canLeave: async () => { seen.push("files"); return true; },
    prepareClose: async () => { seen.push("saved"); },
    destroy: async () => { seen.push("destroy"); },
    onClosing: (value) => seen.push(value ? "locked" : "unlocked"),
    onError: async (error) => { seen.push(error.message); },
    ...overrides,
  });
  return { close, seen };
}

test("declining to stop a chat leaves the workspace open", async () => {
  const { close, seen } = setup({ isBusy: () => true, confirmStop: async () => false });
  await close();
  assert.deepEqual(seen, ["locked", "unlocked"]);
});

test("keeping unsaved edits neither cancels work nor destroys the window", async () => {
  const { close, seen } = setup({ canLeave: async () => false });
  await close();
  assert.deepEqual(seen, ["locked", "unlocked"]);
});

test("Quit and window close wait for persistence and share one pending operation", async () => {
  const saving = Promise.withResolvers();
  const { close, seen } = setup({ prepareClose: () => saving.promise });
  const first = close();
  await Promise.resolve();
  await close();
  assert.deepEqual(seen, ["locked", "files"]);
  saving.resolve();
  await first;
  assert.deepEqual(seen, ["locked", "files", "destroy", "unlocked"]);
});

test("a failed save keeps the window open and allows closing to be retried", async () => {
  let attempt = 0;
  const { close, seen } = setup({ prepareClose: async () => {
    if (attempt++ === 0) throw new Error("disk full");
  } });
  await close();
  assert.deepEqual(seen, ["locked", "files", "disk full", "unlocked"]);
  await close();
  assert.deepEqual(seen.slice(4), ["locked", "files", "destroy", "unlocked"]);
});

test("a failed confirmation keeps the window open", async () => {
  const { close, seen } = setup({ isBusy: () => true, confirmStop: async () => {
    throw new Error("dialog unavailable");
  } });
  await close();
  assert.deepEqual(seen, ["locked", "dialog unavailable", "unlocked"]);
});
