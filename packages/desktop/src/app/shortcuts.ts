type KeyEvent = Pick<
  KeyboardEvent,
  | "key"
  | "metaKey"
  | "ctrlKey"
  | "altKey"
  | "shiftKey"
  | "repeat"
  | "isComposing"
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

type EnterEvent = Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing">;

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
    !event.isComposing &&
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
): AgentShortcut | null {
  if (event.altKey || event.shiftKey || event.repeat) {
    return null;
  }

  const key = event.key.toLowerCase();
  if (view === "terminal") {
    return event.metaKey && !event.ctrlKey && key === "n"
      ? "new-terminal"
      : null;
  }
  if (view === "files" && event.metaKey !== event.ctrlKey && key === "p") {
    return "quick-open";
  }
  if (view !== "chat") {
    return null;
  }

  if (event.metaKey && !event.ctrlKey && key === "n") return "new-session";
  if (event.metaKey && !event.ctrlKey && key === "x") {
    return hasTextSelection(event.target) ? null : "close-session";
  }
  if (event.metaKey && !event.ctrlKey && key === "r") return "history";
  if (event.ctrlKey && !event.metaKey && key === "c" && running) return "stop";
  // Escape stops like Ctrl+C, unless it is cancelling IME input or a popover
  // (e.g. find) already consumed it.
  if (
    key === "escape" &&
    running &&
    !event.isComposing &&
    !event.defaultPrevented
  ) {
    return "stop";
  }
  return null;
}
