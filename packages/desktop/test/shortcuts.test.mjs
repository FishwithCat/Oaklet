import assert from "node:assert/strict";
import test from "node:test";
import {
  agentShortcut,
  createCompositionGuard,
  detectPlatform,
  isImeEvent,
  isSendEnter,
  previewFontShortcut,
  primaryKeyLabel,
} from "../src/app/shortcuts.ts";

const press = (overrides) => ({
  key: "",
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  repeat: false,
  ...overrides,
});

test("Command or Control with +, -, and 0 changes the preview font size", () => {
  assert.equal(previewFontShortcut(press({ key: "+", metaKey: true })), 1);
  assert.equal(previewFontShortcut(press({ key: "=", metaKey: true })), 1);
  assert.equal(previewFontShortcut(press({ key: "+", ctrlKey: true })), 1);
  assert.equal(previewFontShortcut(press({ key: "-", metaKey: true })), -1);
  assert.equal(previewFontShortcut(press({ key: "_", metaKey: true, shiftKey: true })), -1);
  assert.equal(previewFontShortcut(press({ key: "-", ctrlKey: true })), -1);
  assert.equal(previewFontShortcut(press({ key: "+" })), 0);
  assert.equal(previewFontShortcut(press({ key: "-", altKey: true, metaKey: true })), 0);
  assert.equal(previewFontShortcut(press({ key: "0", metaKey: true })), "reset");
  assert.equal(previewFontShortcut(press({ key: "0", ctrlKey: true })), "reset");
  assert.equal(previewFontShortcut(press({ key: "0" })), 0);
});

test("Command+N opens a new session in chat and a new tab in the terminal", () => {
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", false, "mac"),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", true, "mac"),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "terminal", false, "mac"),
    "new-terminal",
  );
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "terminal", false, "mac"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "git", false, "mac"),
    null,
  );
  assert.equal(agentShortcut(press({ key: "n" }), "chat", false, "mac"), null);
});

test("Windows uses Control for the same session shortcuts", () => {
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "chat", false, "windows"),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "terminal", false, "windows"),
    "new-terminal",
  );
  assert.equal(
    agentShortcut(press({ key: "r", ctrlKey: true }), "chat", false, "windows"),
    "history",
  );
  assert.equal(
    agentShortcut(press({ key: "w", ctrlKey: true }), "chat", false, "windows"),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "w", ctrlKey: true }), "terminal", false, "windows"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "x", ctrlKey: true }), "chat", false, "windows"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", false, "windows"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "w", ctrlKey: true, shiftKey: true }), "chat", false, "windows"),
    null,
  );
});

test("Windows keeps Control+X and Control+C for editing", () => {
  assert.equal(
    agentShortcut(
      press({ key: "c", ctrlKey: true, target: { selectionStart: 0, selectionEnd: 4 } }),
      "chat",
      true,
      "windows",
    ),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "c", ctrlKey: true }), "chat", true, "windows"),
    "stop",
  );
  assert.equal(
    agentShortcut(press({ key: "v", ctrlKey: true }), "chat", false, "windows"),
    null,
  );
});

test("the primary key label follows the platform", () => {
  assert.equal(primaryKeyLabel("mac"), "⌘");
  assert.equal(primaryKeyLabel("windows"), "Ctrl");
  assert.equal(primaryKeyLabel("linux"), "Ctrl");
  assert.equal(detectPlatform(), "mac");
});

test("Linux reuses the Control shortcuts", () => {
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "chat", false, "linux"),
    "new-session",
  );
  assert.equal(
    agentShortcut(press({ key: "w", ctrlKey: true }), "chat", false, "linux"),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "n", ctrlKey: true }), "terminal", false, "linux"),
    "new-terminal",
  );
  assert.equal(
    agentShortcut(press({ key: "n", metaKey: true }), "chat", false, "linux"),
    null,
  );
});

test("Command+R opens session history only in the chat view", () => {
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true }), "chat", false, "mac"),
    "history",
  );
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true }), "terminal", false, "mac"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "r", metaKey: true, shiftKey: true }), "chat", false, "mac"),
    null,
  );
});

test("Command+P opens file search only in the files view", () => {
  assert.equal(
    agentShortcut(press({ key: "p", metaKey: true }), "files", false),
    "quick-open",
  );
  assert.equal(
    agentShortcut(press({ key: "p", ctrlKey: true }), "files", false),
    "quick-open",
  );
  assert.equal(
    agentShortcut(press({ key: "p", metaKey: true }), "chat", false),
    null,
  );
  assert.equal(
    agentShortcut(
      press({ key: "p", metaKey: true, shiftKey: true }),
      "files",
      false,
    ),
    null,
  );
});

test("Command+X closes the active chat tab unless text is selected", () => {
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "chat", false, "mac"),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "chat", true, "mac"),
    "close-session",
  );
  assert.equal(
    agentShortcut(press({ key: "x", metaKey: true }), "terminal", false, "mac"),
    null,
  );
  assert.equal(
    agentShortcut(press({ key: "x", ctrlKey: true }), "chat", false, "mac"),
    null,
  );
  assert.equal(
    agentShortcut(
      press({ key: "x", metaKey: true, shiftKey: true }),
      "chat",
      false,
      "mac",
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
      "mac",
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
      "mac",
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
  assert.equal(
    agentShortcut(press({ key: "Escape", keyCode: 229 }), "chat", true),
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
  // WebKit can also omit isComposing and keep only the legacy 229 key code.
  assert.equal(isImeEvent({ ...enter, keyCode: 229 }), true);
  assert.equal(isSendEnter({ ...enter, keyCode: 229 }, false), false);
});

test("Composition guard holds through the closing IME keydown", () => {
  let now = 1000;
  const guard = createCompositionGuard(() => now);
  const enter = press({ key: "Enter" });

  assert.equal(guard.active(), false);
  guard.start();
  assert.equal(isSendEnter(enter, guard.active()), false);

  guard.end();
  now += 5;
  // Safari delivers the confirming keydown just after compositionend.
  assert.equal(isSendEnter(enter, guard.active()), false);

  now += 20;
  assert.equal(isSendEnter({ ...enter, keyCode: 229 }, guard.active()), false);
  assert.equal(isSendEnter(enter, guard.active()), true);

  guard.start();
  assert.equal(guard.active(), true);
  guard.reset();
  assert.equal(guard.active(), false);
});
