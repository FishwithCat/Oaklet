/**
 * One-time move of the history that older builds kept in `localStorage`.
 *
 * Keys are removed only after the backend accepted the import, so a failed
 * upgrade can retry on the next launch.
 */

import type { Item } from "./chat-model";

export type LegacySession = {
  id: string;
  title: string;
  updatedAt: number;
  messages: unknown[];
  items: Item[];
  context?: { used: number; window: number };
};

export type ImportEntry = {
  workspace: string;
  sessions: LegacySession[];
};

const STORAGE_KEY = "oaklet.history.v1";
const SCOPED_PREFIX = `${STORAGE_KEY}:`;

function isLegacySession(value: unknown): value is LegacySession {
  if (!value || typeof value !== "object") return false;
  const session = value as Partial<LegacySession>;
  return (
    typeof session.id === "string" &&
    typeof session.title === "string" &&
    typeof session.updatedAt === "number" &&
    Array.isArray(session.messages) &&
    Array.isArray(session.items)
  );
}

export function parseLegacySessions(raw: string | null): LegacySession[] {
  try {
    const parsed: unknown = JSON.parse(raw ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isLegacySession);
  } catch {
    return [];
  }
}

/**
 * Read every scoped history key. The unscoped key predates workspace scoping
 * and belongs to the workspace active during the upgrade.
 */
export function collectLegacyEntries(
  storage: Pick<Storage, "length" | "key" | "getItem">,
  currentWorkspace: string,
): { keys: string[]; entries: ImportEntry[] } {
  const keys: string[] = [];
  const byWorkspace = new Map<string, LegacySession[]>();

  for (let index = 0; index < storage.length; index += 1) {
    const key = storage.key(index);
    if (!key) continue;

    let workspace = "";
    if (key === STORAGE_KEY) workspace = currentWorkspace;
    else if (key.startsWith(SCOPED_PREFIX)) {
      workspace = key.slice(SCOPED_PREFIX.length);
    }
    if (!workspace) continue;

    keys.push(key);
    const sessions = parseLegacySessions(storage.getItem(key));
    if (sessions.length === 0) continue;
    byWorkspace.set(workspace, [...(byWorkspace.get(workspace) ?? []), ...sessions]);
  }

  return {
    keys,
    entries: [...byWorkspace.entries()].map(([workspace, sessions]) => ({
      workspace,
      sessions,
    })),
  };
}

export async function importLegacySessions(
  storage: Pick<Storage, "length" | "key" | "getItem" | "removeItem">,
  currentWorkspace: string,
  importSessions: (entries: ImportEntry[]) => Promise<number>,
): Promise<number> {
  const { keys, entries } = collectLegacyEntries(storage, currentWorkspace);
  if (keys.length === 0) return 0;

  const imported = entries.length > 0 ? await importSessions(entries) : 0;
  for (const key of keys) storage.removeItem(key);
  return imported;
}
