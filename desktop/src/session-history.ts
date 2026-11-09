import type { Item } from "./chat-model";

export type SessionSnapshot = {
  id: string;
  title: string;
  updatedAt: number;
  messages: unknown[];
  items: Item[];
};

const STORAGE_KEY = "oaklet.history.v1";
const MAX_SESSIONS = 20;

export function sessionTitle(items: Item[]): string {
  const first = items.find((item) => item.kind === "user");
  if (!first || first.kind !== "user") return "";
  const text = first.text.replace(/\s+/g, " ").trim();
  return text.length > 60 ? `${text.slice(0, 59)}…` : text;
}

export function mergeSessions(
  snapshot: SessionSnapshot,
  sessions: SessionSnapshot[],
  max = MAX_SESSIONS,
): SessionSnapshot[] {
  return [snapshot, ...sessions.filter((session) => session.id !== snapshot.id)]
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, max);
}

function isSnapshot(value: unknown): value is SessionSnapshot {
  if (!value || typeof value !== "object") return false;
  const snapshot = value as Partial<SessionSnapshot>;
  return (
    typeof snapshot.id === "string" &&
    typeof snapshot.title === "string" &&
    typeof snapshot.updatedAt === "number" &&
    Array.isArray(snapshot.messages) &&
    Array.isArray(snapshot.items)
  );
}

export function loadSessions(): SessionSnapshot[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(isSnapshot)
      .sort((a, b) => b.updatedAt - a.updatedAt);
  } catch {
    return [];
  }
}

/** ponytail: localStorage holds the last 20 sessions; drop the oldest if outputs outgrow the quota. */
export function saveSession(snapshot: SessionSnapshot): SessionSnapshot[] {
  let sessions = mergeSessions(snapshot, loadSessions());
  while (sessions.length > 0) {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(sessions));
      return sessions;
    } catch {
      sessions = sessions.slice(0, -1);
    }
  }
  return loadSessions();
}
