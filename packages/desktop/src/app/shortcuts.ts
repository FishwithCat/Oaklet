type KeyEvent = Pick<
  KeyboardEvent,
  | "key"
  | "metaKey"
  | "ctrlKey"
  | "altKey"
  | "shiftKey"
  | "repeat"
  | "isComposing"
  | "keyCode"
  | "defaultPrevented"
  | "target"
>;

export type AgentShortcut =
  | "new-session"
  | "close-session"
  | "new-terminal"
  | "quick-open"
  | "history"
  | "stop";

export type ShortcutPlatform = "mac" | "windows" | "linux";

export function detectPlatform(): ShortcutPlatform {
  if (typeof navigator === "undefined") return "mac";
  const text = `${navigator.userAgent ?? ""} ${navigator.platform ?? ""}`;
  if (/mac/i.test(text)) return "mac";
  return /linux/i.test(text) ? "linux" : "windows";
}

export function primaryModifier(
  event: KeyEvent,
  platform: ShortcutPlatform = detectPlatform(),
): boolean {
  return platform === "mac"
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
}

export function primaryKeyLabel(
  platform: ShortcutPlatform = detectPlatform(),
): string {
  return platform === "mac" ? "⌘" : "Ctrl";
}

type EnterEvent = Pick<
  KeyboardEvent,
  "key" | "shiftKey" | "isComposing" | "keyCode"
>;

/** Safari keeps the guard alive briefly after compositionend. */
const COMPOSITION_SETTLE_MS = 10;

/**
 * WebKit can report the closing IME keydown with isComposing cleared; keyCode
 * 229 is the legacy signal those engines still emit for composition keys.
 */
export function isImeEvent(
  event: Pick<KeyboardEvent, "isComposing" | "keyCode">,
): boolean {
  return event.isComposing || event.keyCode === 229;
}

/**
 * Composition lifetime for one input. `end` keeps the guard active for a short
 * window because Safari fires compositionend before the confirming keydown.
 */
export function createCompositionGuard(
  now: () => number = () => performance.now(),
) {
  let composing = false;
  let settleUntil = 0;
  return {
    start() {
      composing = true;
      settleUntil = 0;
    },
    end() {
      composing = false;
      settleUntil = now() + COMPOSITION_SETTLE_MS;
    },
    reset() {
      composing = false;
      settleUntil = 0;
    },
    active() {
      return composing || now() < settleUntil;
    },
  };
}

/** Command/Ctrl with + or - adjusts the markdown preview font size; 0 resets it. */
export function previewFontShortcut(event: KeyEvent): -1 | 0 | 1 | "reset" {
  if ((!event.metaKey && !event.ctrlKey) || event.altKey) return 0;
  // Accept the unshifted keys that some layouts report for Command + "+".
  if (event.key === "+" || event.key === "=") return 1;
  if (event.key === "-" || event.key === "_") return -1;
  if (event.key === "0") return "reset";
  return 0;
}

/**
 * Safari fires compositionend before the Enter that confirms an IME candidate,
 * so that keydown reports isComposing=false; `composing` keeps the guard alive
 * until the confirming keydown has been dispatched.
 */
export function isSendEnter(event: EnterEvent, composing: boolean): boolean {
  return (
    event.key === "Enter" &&
    !event.shiftKey &&
    !isImeEvent(event) &&
    !composing
  );
}

/** A real selection means Cmd+X is a cut, not a tab close. */
function hasTextSelection(target: EventTarget | null): boolean {
  const field = target as {
    selectionStart?: number | null;
    selectionEnd?: number | null;
  } | null;
  return (
    typeof field?.selectionStart === "number" &&
    typeof field.selectionEnd === "number" &&
    field.selectionStart !== field.selectionEnd
  );
}

/** Chat, terminal, and file shortcuts; other views keep their own key handling (e.g. terminal Ctrl+C). */
export function agentShortcut(
  event: KeyEvent,
  view: string,
  running: boolean,
  platform: ShortcutPlatform = detectPlatform(),
): AgentShortcut | null {
  if (event.altKey || event.shiftKey || event.repeat) {
    return null;
  }

  const key = event.key.toLowerCase();
  const primary = primaryModifier(event, platform);
  if (view === "terminal") {
    return primary && key === "n" ? "new-terminal" : null;
  }
  if (view === "files" && event.metaKey !== event.ctrlKey && key === "p") {
    return "quick-open";
  }
  if (view !== "chat") {
    return null;
  }

  if (primary && key === "n") return "new-session";
  if (primary && key === (platform === "mac" ? "x" : "w")) {
    return hasTextSelection(event.target) ? null : "close-session";
  }
  if (primary && key === "r") return "history";
  if (
    event.ctrlKey &&
    !event.metaKey &&
    key === "c" &&
    running &&
    !hasTextSelection(event.target)
  ) {
    return "stop";
  }
  // Escape stops like Ctrl+C, unless it is cancelling IME input or a popover
  // (e.g. find) already consumed it.
  if (
    key === "escape" &&
    running &&
    !isImeEvent(event) &&
    !event.defaultPrevented
  ) {
    return "stop";
  }
  return null;
}
