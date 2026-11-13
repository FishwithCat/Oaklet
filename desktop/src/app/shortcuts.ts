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
  | "history"
  | "stop";

type EnterEvent = Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing">;

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

/** Chat and terminal shortcuts; other views keep their own key handling (e.g. terminal Ctrl+C). */
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
