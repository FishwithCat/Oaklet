type KeyEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "repeat"
>;

export type AgentShortcut = "new-session" | "stop";

/** Chat-view shortcuts; other views keep their own key handling (e.g. terminal Ctrl+C). */
export function agentShortcut(
  event: KeyEvent,
  view: string,
  running: boolean,
): AgentShortcut | null {
  if (view !== "chat" || event.altKey || event.shiftKey || event.repeat) {
    return null;
  }

  const key = event.key.toLowerCase();
  if (event.metaKey && !event.ctrlKey && key === "n") return "new-session";
  if (event.ctrlKey && !event.metaKey && key === "c" && running) return "stop";
  return null;
}
