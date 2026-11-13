import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { createStreamBatch } from "./stream-batch";
import { isBlankTab, type Item } from "./chat-model";
import { sentAttachments, type Attachment } from "./attachments";
import {
  appendNotice,
  applyApproval,
  applyApprovalResolved,
  applyContext,
  applyDeltas,
  applyToolResult,
  applyToolStart,
  settleRun,
  toChatTabInfo,
  withItems,
  type SessionSummary,
  type SessionView,
  type StreamDelta,
} from "./session-model.ts";
import { subscribeAgentEvents } from "./agent-events.ts";

type OpenTab = { id: string; number: number };

type SendResult = { runId: string; session: SessionView };

type CompactResult = {
  session: SessionView;
  before: number;
  after: number;
  compacted: boolean;
};

/**
 * Chat tabs backed by Rust. The backend owns sessions, runs, and the transcript;
 * this hook keeps a render cache hydrated from command responses and events.
 * Open tabs are client UI state and are never written back to storage.
 */
export function useAgentSession({
  workspace,
  enabled,
  onWorkspaceRefresh,
}: {
  workspace: string;
  enabled: boolean;
  onWorkspaceRefresh: () => void;
}) {
  const [sessions, setSessions] = useState<Record<string, SessionView>>({});
  const [openTabs, setOpenTabs] = useState<OpenTab[]>([]);
  const [activeTabId, setActiveTabId] = useState("");
  const [allowAll, setAllowAll] = useState(false);
  const [history, setHistory] = useState<SessionSummary[]>([]);
  const [compacting, setCompacting] = useState(false);

  const sessionsRef = useRef(sessions);
  const openTabsRef = useRef(openTabs);
  const activeTabIdRef = useRef(activeTabId);
  const workspaceRef = useRef(workspace);
  const tabCountRef = useRef(0);
  const compactingRef = useRef(false);
  const startedForRef = useRef<string | null>(null);
  const onWorkspaceRefreshRef = useRef(onWorkspaceRefresh);

  function commitSessions(next: Record<string, SessionView>) {
    sessionsRef.current = next;
    setSessions(next);
  }

  function putSession(view: SessionView) {
    commitSessions({ ...sessionsRef.current, [view.id]: view });
  }

  function updateSession(
    id: string,
    update: (view: SessionView) => SessionView,
  ) {
    const current = sessionsRef.current[id];
    if (!current) return;
    putSession(update(current));
  }

  function commitTabs(next: OpenTab[]) {
    openTabsRef.current = next;
    setOpenTabs(next);
  }

  function activate(id: string) {
    activeTabIdRef.current = id;
    setActiveTabId(id);
  }

  function reset() {
    commitSessions({});
    commitTabs([]);
    activate("");
    tabCountRef.current = 0;
    setHistory([]);
  }

  function addTab(view: SessionView) {
    tabCountRef.current += 1;
    commitTabs([...openTabsRef.current, { id: view.id, number: tabCountRef.current }]);
    putSession(view);
    activate(view.id);
  }

  function showError(text: string) {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (tab) updateSession(tab.id, (view) => appendNotice(view, "error", text));
    else console.error(text);
  }

  function refreshHistory() {
    const workspace = workspaceRef.current;
    if (!workspace) return;
    invoke<SessionSummary[]>("session_list", { workspace })
      .then((list) => {
        if (workspaceRef.current === workspace) setHistory(list);
      })
      .catch(() => {});
  }

  useEffect(() => {
    onWorkspaceRefreshRef.current = onWorkspaceRefresh;
  }, [onWorkspaceRefresh]);

  useEffect(() => {
    if (!enabled || !workspace) return;
    // StrictMode runs this effect twice; the first async pass owns the work.
    if (startedForRef.current === workspace) return;
    startedForRef.current = workspace;

    const switched = workspaceRef.current !== workspace;
    workspaceRef.current = workspace;
    if (switched) reset();

    void (async () => {
      try {
        const list = await invoke<SessionSummary[]>("session_list", { workspace });
        if (workspaceRef.current !== workspace) return;
        setHistory(list);
        if (openTabsRef.current.length > 0) return;

        const created = await invoke<SessionView>("session_create", { workspace });
        if (workspaceRef.current !== workspace) return;
        if (openTabsRef.current.length > 0) return;
        addTab(created);
      } catch (error) {
        startedForRef.current = null;
        console.error(`Could not start a chat: ${errorText(error)}`);
      }
    })();
  }, [workspace, enabled]);

  function loadAllowance() {
    invoke<boolean>("command_allow_all_active")
      .then(setAllowAll)
      .catch(() => setAllowAll(false));
  }

  useEffect(() => {
    loadAllowance();
  }, []);

  useEffect(() => {
    let disposed = false;
    let release: (() => void) | null = null;

    const batch = createStreamBatch((deltas: StreamDelta[]) => {
      const bySession = new Map<string, StreamDelta[]>();
      for (const delta of deltas) {
        const group = bySession.get(delta.sessionId);
        if (group) group.push(delta);
        else bySession.set(delta.sessionId, [delta]);
      }

      for (const [sessionId, group] of bySession) {
        updateSession(sessionId, (tab) =>
          applyDeltas(tab, group[0]!.runId, group),
        );
      }
    });

    function finish(
      sessionId: string,
      session: SessionView | null,
      notice?: { tone: "error" | "muted"; text: string },
    ) {
      const tab = sessionsRef.current[sessionId];
      if (!tab) return;
      putSession(session ?? settleRun(tab, notice));
      refreshHistory();
      onWorkspaceRefreshRef.current();
    }

    subscribeAgentEvents({
      onDelta: (event) => {
        if (disposed) return;
        batch.push({ ...event, kind: "assistant" });
      },
      onReasoning: (event) => {
        if (disposed) return;
        batch.push({ ...event, kind: "reasoning" });
      },
      onToolStart: (event) => {
        if (disposed) return;
        batch.flush();
        updateSession(event.sessionId, (tab) =>
          applyToolStart(tab, event, Date.now()),
        );
      },
      onToolResult: (event) => {
        if (disposed) return;
        updateSession(event.sessionId, (tab) =>
          applyToolResult(tab, event, Date.now()),
        );
      },
      onApproval: (event) => {
        if (disposed) return;
        updateSession(event.sessionId, (tab) => applyApproval(tab, event));
      },
      onApprovalResolved: (event) => {
        if (disposed) return;
        updateSession(event.sessionId, (tab) =>
          applyApprovalResolved(tab, event, Date.now()),
        );
      },
      onDone: (event) => {
        if (disposed) return;
        batch.flush();
        finish(event.sessionId, event.session);
      },
      onContext: (event) => {
        if (disposed) return;
        updateSession(event.sessionId, (tab) => applyContext(tab, event));
      },
      onError: (event) => {
        if (disposed) return;
        batch.flush();
        finish(event.sessionId, event.session, {
          tone: "error",
          text: event.message,
        });
      },
      onStopped: (event) => {
        if (disposed) return;
        batch.flush();
        finish(event.sessionId, event.session, {
          tone: "muted",
          text: "Stopped.",
        });
      },
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((error) => {
        if (!disposed) showError(errorText(error));
      });

    return () => {
      disposed = true;
      batch.clear();
      release?.();
    };
  }, []);

  async function newTab() {
    const workspace = workspaceRef.current;
    if (!workspace) return;
    // The welcome tab is already a new chat; another one would just duplicate it.
    const active = sessionsRef.current[activeTabIdRef.current];
    if (active && isBlankTab(active)) return;
    try {
      const view = await invoke<SessionView>("session_create", { workspace });
      if (workspaceRef.current !== workspace) return;
      addTab(view);
    } catch (error) {
      if (workspaceRef.current === workspace) showError(errorText(error));
    }
  }

  function selectTab(id: string) {
    if (openTabsRef.current.some((tab) => tab.id === id)) activate(id);
  }

  function closeTab(id: string) {
    const index = openTabsRef.current.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    const tab = sessionsRef.current[id];
    if (tab?.running) return;

    const remaining = openTabsRef.current.filter((entry) => entry.id !== id);
    const next = { ...sessionsRef.current };
    delete next[id];
    commitSessions(next);

    if (remaining.length === 0) {
      commitTabs([]);
      void newTab();
      return;
    }

    commitTabs(remaining);
    if (activeTabIdRef.current === id) {
      activate((remaining[index] ?? remaining[index - 1])!.id);
    }
  }

  function send(text: string, attachments: Attachment[] = []): boolean {
    const trimmed = text.trim();
    if ((!trimmed && attachments.length === 0) || !enabled) return false;

    const tab = sessionsRef.current[activeTabIdRef.current];
    if (!tab || tab.running || compactingRef.current) return false;

    const workspaceAtSend = workspaceRef.current;
    const previous = tab;
    const userItem: Item =
      attachments.length > 0
        ? {
            kind: "user",
            text: trimmed,
            attachments: sentAttachments(attachments),
          }
        : { kind: "user", text: trimmed };
    putSession(
      withItems(
        { ...tab, running: true, activeRunId: null },
        [...tab.items, userItem],
      ),
    );

    invoke<SendResult>("session_send", {
      sessionId: tab.id,
      text: trimmed,
      attachments,
    })
      .then(({ runId, session }) => {
        if (workspaceRef.current !== workspaceAtSend) return;
        const current = sessionsRef.current[tab.id];
        // A terminal event may already have replaced the optimistic view.
        if (!current || !current.running) return;
        // The run finished before the command response; its view is final.
        if (!session.running) {
          putSession(session);
          return;
        }
        putSession({
          ...current,
          title: session.title,
          turns: session.turns,
          updatedAt: session.updatedAt,
          running: true,
          activeRunId: runId,
        });
      })
      .catch((error) => {
        if (workspaceRef.current !== workspaceAtSend) return;
        if (!sessionsRef.current[tab.id]) return;
        putSession(appendNotice(previous, "error", errorText(error)));
      });
    return true;
  }

  function stop() {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (tab?.running) void invoke("session_stop", { sessionId: tab.id });
  }

  function compact() {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (!tab || tab.running || compactingRef.current || tab.turns === 0) {
      return;
    }

    compactingRef.current = true;
    setCompacting(true);
    const workspaceAtStart = workspaceRef.current;
    invoke<CompactResult>("session_compact", { sessionId: tab.id })
      .then(({ session, compacted }) => {
        if (workspaceRef.current !== workspaceAtStart) return;
        if (!sessionsRef.current[tab.id]) return;
        putSession(session);
        if (!compacted) {
          updateSession(tab.id, (view) =>
            appendNotice(view, "muted", "Nothing older to compact yet."),
          );
        }
      })
      .catch((error) => {
        if (workspaceRef.current === workspaceAtStart) {
          updateSession(tab.id, (view) =>
            appendNotice(view, "error", errorText(error)),
          );
        }
      })
      .finally(() => {
        compactingRef.current = false;
        setCompacting(false);
      });
  }

  async function restore(id: string) {
    if (openTabsRef.current.some((tab) => tab.id === id)) {
      activate(id);
      return;
    }

    const workspaceAtStart = workspaceRef.current;
    try {
      const view = await invoke<SessionView>("session_get", { id });
      if (workspaceRef.current !== workspaceAtStart) return;
      if (openTabsRef.current.some((tab) => tab.id === id)) {
        activate(id);
        return;
      }

      // Replacing the welcome tab keeps Cmd+R from leaving an empty chat beside
      // the restored session.
      const activeId = activeTabIdRef.current;
      const active = sessionsRef.current[activeId];
      if (activeId && active && isBlankTab(active)) {
        const remaining = openTabsRef.current.filter((tab) => tab.id !== activeId);
        commitTabs(remaining);
        const next = { ...sessionsRef.current };
        delete next[activeId];
        commitSessions(next);
      }
      addTab(view);
    } catch (error) {
      if (workspaceRef.current === workspaceAtStart) showError(errorText(error));
    }
  }

  function respondToApproval(item: ToolItem, choice: ApprovalChoice) {
    if (!item.requestId) return;
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (!tab) return;

    const requestId = item.requestId;
    updateSession(tab.id, (view) =>
      withItems(
        view,
        view.items.map((entry) =>
          entry.kind === "tool" && entry.id === item.id
            ? {
                ...entry,
                status: "running" as const,
                requestId: undefined,
                startedAt: choice === "deny" ? undefined : Date.now(),
              }
            : entry,
        ),
      ),
    );
    invoke("approve_tool", {
      requestId,
      allow: choice !== "deny",
      always: choice === "allowAll",
    })
      .then(() => {
        if (choice === "allowAll") setAllowAll(true);
      })
      .catch((error) => {
        updateSession(tab.id, (view) =>
          appendNotice(view, "error", errorText(error)),
        );
      });
  }

  function revokeAllowAll() {
    invoke("revoke_command_allow_all")
      .then(() => setAllowAll(false))
      .catch((error) => showError(errorText(error)));
  }

  const tabs = openTabs.flatMap(({ id, number }) => {
    const view = sessions[id];
    return view ? [toChatTabInfo(view, number)] : [];
  });
  const resolvedActiveId = sessions[activeTabId]
    ? activeTabId
    : (openTabs[0]?.id ?? "");
  const activeTab = sessions[resolvedActiveId];

  return {
    items: activeTab?.items ?? [],
    running: activeTab?.running ?? false,
    activeRunId: activeTab?.activeRunId ?? null,
    compacting: compacting || (activeTab?.compacting ?? false),
    tabs,
    activeTabId: resolvedActiveId,
    context: activeTab?.context ?? null,
    allowAll,
    history,
    newTab,
    selectTab,
    closeTab,
    send,
    stop,
    compact,
    restore,
    respondToApproval,
    revokeAllowAll,
    reloadAllowance: loadAllowance,
    refreshHistory,
  };
}
