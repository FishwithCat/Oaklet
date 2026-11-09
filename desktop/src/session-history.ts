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

function storageKey(workspace: string): string {
  return `${STORAGE_KEY}:${workspace}`;
}

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

function parseSessions(raw: string | null): SessionSnapshot[] {
  try {
    const parsed: unknown = JSON.parse(raw ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(isSnapshot)
      .sort((a, b) => b.updatedAt - a.updatedAt);
  } catch {
    return [];
  }
}

export function loadSessions(workspace: string): SessionSnapshot[] {
  if (!workspace) return [];
  const key = storageKey(workspace);
  let raw = localStorage.getItem(key);
  if (raw === null) {
    // History saved before it was workspace-scoped belongs to the workspace
    // in use when the app upgrades.
    const legacy = localStorage.getItem(STORAGE_KEY);
    if (legacy !== null) {
      raw = legacy;
      try {
        localStorage.setItem(key, legacy);
        localStorage.removeItem(STORAGE_KEY);
      } catch {
        // Still show the legacy sessions if the scoped copy cannot be written.
      }
    }
  }
  return parseSessions(raw);
}

/** ponytail: localStorage holds the last 20 sessions; drop the oldest if outputs outgrow the quota. */
export function saveSession(
  workspace: string,
  snapshot: SessionSnapshot,
): SessionSnapshot[] {
  if (!workspace) return [];
  let sessions = mergeSessions(snapshot, loadSessions(workspace));
  while (sessions.length > 0) {
    try {
      localStorage.setItem(storageKey(workspace), JSON.stringify(sessions));
      return sessions;
    } catch {
      sessions = sessions.slice(0, -1);
    }
  }
  return loadSessions(workspace);
}
