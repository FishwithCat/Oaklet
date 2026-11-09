import assert from "node:assert/strict";
import test from "node:test";
import { agentShortcut } from "../src/shortcuts.ts";

const press = (overrides) => ({
  key: "",
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  repeat: false,
  ...overrides,
});

test("Command+N opens a new session in chat and a new tab in the terminal", () => {
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", false),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", true),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "terminal", false),
    "new-terminal",
  );
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "terminal", false),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "git", false),
    null,
  );
  assert.equal(agentShortcut(press({ key: "n" }), "chat", false), null);
});

test("Command+R opens session history only in the chat view", () => {
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true }), "chat", false),
    "history",
  );
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true }), "terminal", false),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true, shiftKey: true }), "chat", false),
    null,
  );
});

test("Control+C stops a running session without hijacking the terminal or copy", () => {
  assert.equal(
    agentShortcut(press({ key: "c", ctrlKey: true }), "chat", true),
    "stop",
  );
  assert.equal(
    agentShortcut(press({ key: "c", ctrlKey: true }), "chat", false),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "c", ctrlKey: true }), "terminal", true),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "c", metaKey: true }), "chat", true),
    null,
  );
});
