import assert from "node:assert/strict";
import test from "node:test";
import { agentShortcut, isSendEnter } from "../src/app/shortcuts.ts";

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

test("Command+X closes the active chat tab unless text is selected", () => {
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "chat", false),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "chat", true),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "terminal", false),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "x", ctrlKey: true }), "chat", false),
    null,
  );
  assert.equal(
    agentShortcut(
      press({ key: "x", metaKey: true, shiftKey: true }),
      "chat",
      false,
    ),
    null,
  );
  assert.equal(
    agentShortcut(
      press({
        key: "x",
        metaKey: true,
        target: { selectionStart: 1, selectionEnd: 4 },
      }),
      "chat",
      false,
    ),
    null,
  );
  assert.equal(
    agentShortcut(
      press({
        key: "x",
        metaKey: true,
        target: { selectionStart: 2, selectionEnd: 2 },
      }),
      "chat",
      false,
    ),
    "close-session",
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

test("Escape stops a running session just like Control+C", () => {
  assert.equal(agentShortcut(press({ key: "Escape" }), "chat", true), "stop");
  assert.equal(agentShortcut(press({ key: "Escape" }), "chat", false), null);
  assert.equal(
    agentShortcut(press({ key: "Escape" }), "terminal", true),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "Escape", isComposing: true }), "chat", true),
    null,
  );
  assert.equal(
    agentShortcut(
      press({ key: "Escape", defaultPrevented: true }),
      "chat",
      true,
    ),
    null,
  );
});

test("Enter sends only when no IME composition is active", () => {
  const enter = press({ key: "Enter" });

  assert.equal(isSendEnter(enter, false), true);
  assert.equal(isSendEnter({ ...enter, shiftKey: true }, false), false);
  // Chrome and Firefox report isComposing while the candidate window is open.
  assert.equal(isSendEnter({ ...enter, isComposing: true }, false), false);
  // Safari clears isComposing before the confirming keydown, so the tracked
  // composition flag must still block it.
  assert.equal(isSendEnter(enter, true), false);
});
