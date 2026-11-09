type KeyEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "repeat"
>;

export type AgentShortcut =
  | "new-session"
  | "new-terminal"
  | "history"
  | "stop";

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
  if (event.metaKey && !event.ctrlKey && key === "r") return "history";
  if (event.ctrlKey && !event.metaKey && key === "c" && running) return "stop";
  return null;
}
