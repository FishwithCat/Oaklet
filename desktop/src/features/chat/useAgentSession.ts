import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { createStreamBatch } from "./stream-batch";
import { isBlankTab, type Item } from "./chat-model";
import {
  loadSessions,
  saveSession,
  sessionTitle,
  type SessionSnapshot,
} from "./session-history";
import {
  applyApproval,
  applyDeltas,
  applyToolResult,
  applyToolStart,
  blankTab,
  finishDone,
  finishError,
  finishStopped,
  toChatTabInfo,
  type ChatTabState,
  type StreamDelta,
} from "./session-model.ts";
import { subscribeAgentEvents } from "./agent-events.ts";

function newId(): string {
  return (
    crypto.randomUUID?.() ??
    `session-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  );
}

export function useAgentSession({
  workspace,
  enabled,
  onWorkspaceRefresh,
}: {
  workspace: string;
  enabled: boolean;
  onWorkspaceRefresh: () => void;
}) {
  const firstTabRef = useRef<ChatTabState | null>(null);
  if (!firstTabRef.current) firstTabRef.current = blankTab(newId(), 1);

  const [tabs, setTabs] = useState<ChatTabState[]>(() => [firstTabRef.current!]);
  const [activeTabId, setActiveTabId] = useState(firstTabRef.current.id);
  const [allowAll, setAllowAll] = useState(false);
  const [history, setHistory] = useState<SessionSnapshot[]>([]);

  const tabsRef = useRef(tabs);
  const tabCountRef = useRef(1);
  const streamBatchRef = useRef<ReturnType<typeof createStreamBatch> | null>(null);
  const workspaceRef = useRef(workspace);
  const runWorkspaceRef = useRef(workspace);
  const pendingSendsRef = useRef(new Set<string>());
  const pendingRunEventsRef = useRef(new Map<string, Array<(tabId: string) => void>>());
  const onWorkspaceRefreshRef = useRef(onWorkspaceRefresh);

  function commitTabs(next: (current: ChatTabState[]) => ChatTabState[]) {
    const updated = next(tabsRef.current);
    tabsRef.current = updated;
    setTabs(updated);
  }

  function patchTab(tabId: string, patch: Partial<ChatTabState>) {
    commitTabs((current) =>
      current.map((tab) => (tab.id === tabId ? { ...tab, ...patch } : tab)),
    );
  }

  function addNotice(tabId: string, tone: "error" | "muted", text: string) {
    const notice: Item = { kind: "notice", tone, text };
    commitTabs((current) =>
      current.map((tab) =>
        tab.id === tabId ? { ...tab, items: [...tab.items, notice] } : tab,
      ),
    );
  }

  function routeRunEvent(runId: string, apply: (tabId: string) => void) {
    if (runWorkspaceRef.current !== workspaceRef.current) return;
    const tab = tabsRef.current.find((candidate) => candidate.runId === runId);
    if (tab) {
      apply(tab.id);
      return;
    }
    // The invoke response can arrive after the backend's first events.
    if (pendingSendsRef.current.size > 0) {
      const queued = pendingRunEventsRef.current.get(runId) ?? [];
      queued.push(apply);
      pendingRunEventsRef.current.set(runId, queued);
    }
  }

  function drainRunEvents(runId: string) {
    const queued = pendingRunEventsRef.current.get(runId);
    if (!queued) return;
    pendingRunEventsRef.current.delete(runId);
    const tab = tabsRef.current.find((candidate) => candidate.runId === runId);
    if (!tab) return;
    for (const apply of queued) apply(tab.id);
  }

  function persistTab(tabId: string, nextItems: Item[], messages: unknown[]) {
    const title = sessionTitle(nextItems);
    if (!title) return;
    const tab = tabsRef.current.find((candidate) => candidate.id === tabId);
    if (!tab) return;

    const id = tab.savedSessionId ?? newId();
    const snapshot: SessionSnapshot = {
      id,
      title,
      updatedAt: Date.now(),
      messages,
      items: nextItems,
    };
    patchTab(tabId, { savedSessionId: id });
    setHistory(saveSession(workspaceRef.current, snapshot));
  }

  useEffect(() => {
    const switched = workspaceRef.current !== workspace;
    workspaceRef.current = workspace;
    if (switched) {
      streamBatchRef.current?.clear();
      for (const tab of tabsRef.current) {
        if (tab.runId) {
          void invoke("agent_stop", { runId: tab.runId });
        }
      }
      pendingSendsRef.current.clear();
      pendingRunEventsRef.current.clear();
      tabCountRef.current = 1;
      const replacement = blankTab(newId(), 1);
      commitTabs(() => [replacement]);
      setActiveTabId(replacement.id);
    }
    setHistory(workspace ? loadSessions(workspace) : []);
  }, [workspace]);

  useEffect(() => {
    onWorkspaceRefreshRef.current = onWorkspaceRefresh;
  }, [onWorkspaceRefresh]);

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
      const byRun = new Map<string, StreamDelta[]>();
      for (const delta of deltas) {
        const group = byRun.get(delta.runId);
        if (group) group.push(delta);
        else byRun.set(delta.runId, [delta]);
      }

      for (const [runId, group] of byRun) {
        routeRunEvent(runId, (tabId) => {
          commitTabs((current) =>
            current.map((tab) =>
              tab.id === tabId ? applyDeltas(tab, runId, group) : tab,
            ),
          );
        });
      }
    });
    streamBatchRef.current = batch;

    const sameWorkspace = () => runWorkspaceRef.current === workspaceRef.current;

    subscribeAgentEvents({
      onDelta: (event) => {
        if (disposed || !sameWorkspace()) return;
        batch.push({ ...event, kind: "assistant" });
      },
      onReasoning: (event) => {
        if (disposed || !sameWorkspace()) return;
        batch.push({ ...event, kind: "reasoning" });
      },
      onToolStart: (event) => {
        if (disposed) return;
        batch.flush();
        routeRunEvent(event.runId, (tabId) => {
          const startedAt = Date.now();
          commitTabs((current) =>
            current.map((tab) =>
              tab.id === tabId ? applyToolStart(tab, event, startedAt) : tab,
            ),
          );
        });
      },
      onToolResult: (event) => {
        if (disposed) return;
        routeRunEvent(event.runId, (tabId) => {
          const finishedAt = Date.now();
          commitTabs((current) =>
            current.map((tab) =>
              tab.id === tabId
                ? applyToolResult(tab, event, finishedAt)
                : tab,
            ),
          );
        });
      },
      onApproval: (event) => {
        if (disposed) return;
        routeRunEvent(event.runId, (tabId) => {
          commitTabs((current) =>
            current.map((tab) =>
              tab.id === tabId ? applyApproval(tab, event) : tab,
            ),
          );
        });
      },
      onDone: (event) => {
        if (disposed) return;
        batch.flush();
        routeRunEvent(event.runId, (tabId) => {
          const tab = tabsRef.current.find((candidate) => candidate.id === tabId);
          if (!tab) return;
          const next = finishDone(tab, event.messages);
          commitTabs((current) =>
            current.map((candidate) =>
              candidate.id === tabId ? next : candidate,
            ),
          );
          persistTab(tabId, next.items, event.messages);
          onWorkspaceRefreshRef.current();
        });
      },
      onError: (event) => {
        if (disposed) return;
        batch.flush();
        routeRunEvent(event.runId, (tabId) => {
          const tab = tabsRef.current.find((candidate) => candidate.id === tabId);
          if (!tab) return;
          commitTabs((current) =>
            current.map((candidate) =>
              candidate.id === tabId
                ? finishError(candidate, event.message)
                : candidate,
            ),
          );
          onWorkspaceRefreshRef.current();
        });
      },
      onStopped: (event) => {
        if (disposed) return;
        batch.flush();
        routeRunEvent(event.runId, (tabId) => {
          commitTabs((current) =>
            current.map((candidate) =>
              candidate.id === tabId ? finishStopped(candidate) : candidate,
            ),
          );
          onWorkspaceRefreshRef.current();
        });
      },
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((error) => {
        if (disposed) return;
        const tab = tabsRef.current[0];
        if (tab) addNotice(tab.id, "error", errorText(error));
      });

    return () => {
      disposed = true;
      batch.clear();
      release?.();
    };
  }, []);

  function newTab() {
    tabCountRef.current += 1;
    const tab = blankTab(newId(), tabCountRef.current);
    commitTabs((current) => [...current, tab]);
    setActiveTabId(tab.id);
  }

  function selectTab(id: string) {
    if (tabsRef.current.some((tab) => tab.id === id)) setActiveTabId(id);
  }

  function closeTab(id: string) {
    const current = tabsRef.current;
    const index = current.findIndex((tab) => tab.id === id);
    if (index < 0 || current[index].running) return;

    const remaining = current.filter((tab) => tab.id !== id);
    if (remaining.length === 0) {
      tabCountRef.current += 1;
      const replacement = blankTab(newId(), tabCountRef.current);
      commitTabs(() => [replacement]);
      setActiveTabId(replacement.id);
      return;
    }

    commitTabs(() => remaining);
    setActiveTabId((selected) =>
      selected === id ? (remaining[index] ?? remaining[index - 1]).id : selected,
    );
  }

  function send(text: string): boolean {
    const trimmed = text.trim();
    if (!trimmed || !enabled) return false;

    const tab = tabsRef.current.find((candidate) => candidate.id === activeTabId);
    if (!tab || tab.running) return false;

    const nextMessages = [
      ...tab.messages,
      { role: "user", content: trimmed },
    ];
    const userItem: Item = { kind: "user", text: trimmed };
    const title = sessionTitle([...tab.items, userItem]);
    runWorkspaceRef.current = workspaceRef.current;
    pendingSendsRef.current.add(tab.id);
    commitTabs((current) =>
      current.map((candidate) =>
        candidate.id === tab.id
          ? {
              ...candidate,
              title,
              messages: nextMessages,
              items: [...candidate.items, userItem],
              running: true,
              completed: false,
              activeRunId: null,
              runId: null,
            }
          : candidate,
      ),
    );

    invoke<string>("agent_send", { messages: nextMessages })
      .then((runId) => {
        pendingSendsRef.current.delete(tab.id);
        if (runWorkspaceRef.current !== workspaceRef.current) {
          void invoke("agent_stop", { runId });
          return;
        }
        if (!tabsRef.current.some((candidate) => candidate.id === tab.id)) {
          void invoke("agent_stop", { runId });
          return;
        }
        patchTab(tab.id, { runId });
        drainRunEvents(runId);
      })
      .catch((error) => {
        pendingSendsRef.current.delete(tab.id);
        if (runWorkspaceRef.current !== workspaceRef.current) return;
        patchTab(tab.id, {
          running: false,
          activeRunId: null,
          runId: null,
        });
        addNotice(tab.id, "error", errorText(error));
      });
    return true;
  }

  function stop() {
    const tab = tabsRef.current.find((candidate) => candidate.id === activeTabId);
    if (tab?.runId) {
      void invoke("agent_stop", { runId: tab.runId });
    }
  }

  function restore(id: string) {
    const snapshot = history.find((session) => session.id === id);
    if (!snapshot) return;

    const open = tabsRef.current.find((tab) => tab.savedSessionId === id);
    if (open) {
      setActiveTabId(open.id);
      return;
    }

    tabCountRef.current += 1;
    const fresh = blankTab(newId(), tabCountRef.current);
    const tab: ChatTabState = {
      ...fresh,
      title: snapshot.title || sessionTitle(snapshot.items) || fresh.title,
      messages: snapshot.messages,
      items: snapshot.items,
      savedSessionId: snapshot.id,
    };
    // Replacing the welcome tab keeps Cmd+R from leaving an empty chat beside
    // the restored session.
    commitTabs((current) => [
      ...current.filter(
        (candidate) =>
          candidate.id !== activeTabId || !isBlankTab(candidate),
      ),
      tab,
    ]);
    setActiveTabId(tab.id);
  }

  function respondToApproval(item: ToolItem, choice: ApprovalChoice) {
    if (!item.requestId) return;
    const tab = tabsRef.current.find((candidate) => candidate.id === activeTabId);
    if (!tab) return;

    const requestId = item.requestId;
    patchTab(tab.id, {
      items: tab.items.map((entry) =>
        entry.kind === "tool" && entry.id === item.id
          ? {
              ...entry,
              status: "running",
              requestId: undefined,
              startedAt: choice === "deny" ? undefined : Date.now(),
            }
          : entry,
      ),
    });
    invoke("approve_tool", {
      requestId,
      allow: choice !== "deny",
      always: choice === "allowAll",
    })
      .then(() => {
        if (choice === "allowAll") setAllowAll(true);
      })
      .catch((error) => {
        addNotice(tab.id, "error", errorText(error));
      });
  }

  function revokeAllowAll() {
    invoke("revoke_command_allow_all")
      .then(() => setAllowAll(false))
      .catch((error) => {
        const tab = tabsRef.current[0];
        if (tab) addNotice(tab.id, "error", errorText(error));
      });
  }

  const activeTab = tabs.find((tab) => tab.id === activeTabId) ?? tabs[0];
  return {
    items: activeTab?.items ?? [],
    running: activeTab?.running ?? false,
    activeRunId: activeTab?.activeRunId ?? null,
    tabs: tabs.map(toChatTabInfo),
    activeTabId: activeTab?.id ?? "",
    allowAll,
    history,
    newTab,
    selectTab,
    closeTab,
    send,
    stop,
    restore,
    respondToApproval,
    revokeAllowAll,
    reloadAllowance: loadAllowance,
  };
}
