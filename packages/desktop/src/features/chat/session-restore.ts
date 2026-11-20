import type { SessionSummary } from "./session-model.ts";

/** localStorage key prefix; the canonical workspace path is appended. */
export const LAST_SESSION_KEY_PREFIX = "oaklet:last-session:";

/** The small part of Storage this module needs; lets tests use a plain map. */
export type PreferenceStore = Pick<Storage, "getItem" | "setItem">;

function defaultStore(): PreferenceStore | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // Storage can be unavailable or blocked; sessions still work without it.
    return null;
  }
}

export function lastSessionKey(workspace: string): string {
  return `${LAST_SESSION_KEY_PREFIX}${workspace}`;
}

/** The session id remembered for this workspace, or null when unreadable. */
export function readLastSessionId(
  workspace: string,
  store: PreferenceStore | null = defaultStore(),
): string | null {
  if (!workspace || !store) return null;
  try {
    return store.getItem(lastSessionKey(workspace)) || null;
  } catch {
    return null;
  }
}

/** Remember the selected session; a storage failure leaves chat unaffected. */
export function rememberLastSession(
  workspace: string,
  id: string,
  store: PreferenceStore | null = defaultStore(),
): void {
  if (!workspace || !id || !store) return;
  try {
    store.setItem(lastSessionKey(workspace), id);
  } catch {
    // Ignore quota and permission failures: the preference is best-effort.
  }
}

/**
 * Pick the startup session from history: the remembered one when it is still
 * listed (even if another session is newer), otherwise the newest row. An empty
 * history means the workspace should start with a blank chat instead.
 */
export function pickStartupSession(
  list: SessionSummary[],
  rememberedId: string | null,
): SessionSummary | null {
  if (list.length === 0) return null;
  if (rememberedId) {
    const remembered = list.find((session) => session.id === rememberedId);
    if (remembered) return remembered;
  }
  return list[0] ?? null;
}
