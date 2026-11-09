import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { createStreamBatch } from "./stream-batch";
import { settleStreaming, type Item } from "./chat-model";
import {
  loadSessions,
  saveSession,
  sessionTitle,
  type SessionSnapshot,
} from "./session-history";

type DeltaEvent = { runId: string; text: string };
type ReasoningEvent = { runId: string; text: string };
type ToolStartEvent = {
  runId: string;
  id: string;
  name: string;
  summary: string;
};
type ToolResultEvent = { runId: string; id: string; ok: boolean; output: string };
type ApprovalEvent = {
  runId: string;
  id: string;
  requestId: string;
  tool: string;
  prompt: string;
  detail: string;
};
type DoneEvent = { runId: string; messages: unknown[] };
type ErrorEvent = { runId: string; message: string };
type RunIdEvent = { runId: string };

export type ChatTabInfo = {
  id: string;
  title: string;
  running: boolean;
};

type ChatTabState = {
  id: string;
  title: string;
  running: boolean;
  items: Item[];
  messages: unknown[];
  activeRunId: string | null;
  runId: string | null;
  savedSessionId: string | null;
};

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}

function newId(): string {
  return (
    crypto.randomUUID?.() ??
    `session-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  );
}

function blankTab(number: number): ChatTabState {
  return {
    id: newId(),
    title: `Chat ${number}`,
    running: false,
    items: [],
    messages: [],
    activeRunId: null,
    runId: null,
    savedSessionId: null,
  };
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
  if (!firstTabRef.current) firstTabRef.current = blankTab(1);

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
      const replacement = blankTab(1);
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
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;

    const batch = createStreamBatch((deltas) => {
      const byRun = new Map<string, typeof deltas>();
      for (const delta of deltas) {
        const group = byRun.get(delta.runId);
        if (group) group.push(delta);
        else byRun.set(delta.runId, [delta]);
      }

      for (const [runId, group] of byRun) {
        routeRunEvent(runId, (tabId) => {
          commitTabs((current) =>
            current.map((tab) => {
              if (tab.id !== tabId) return tab;
              let items = tab.items;
              for (const delta of group) {
                const last = items[items.length - 1];
                if (
                  last?.kind === delta.kind &&
                  last.streaming &&
                  (last.kind === "assistant" || last.runId === delta.runId)
                ) {
                  items = [
                    ...items.slice(0, -1),
                    { ...last, text: last.text + delta.text },
                  ];
                } else {
                  items = [
                    ...settleStreaming(items),
                    { ...delta, streaming: true },
                  ];
                }
              }
              return { ...tab, items, activeRunId: runId };
            }),
          );
        });
      }
    });
    streamBatchRef.current = batch;

    const register = async () => {
      const subscriptions = await Promise.all([
        listen<DeltaEvent>("agent:delta", (event) => {
          if (runWorkspaceRef.current !== workspaceRef.current) return;
          batch.push({ ...event.payload, kind: "assistant" });
        }),
        listen<ReasoningEvent>("agent:reasoning", (event) => {
          if (runWorkspaceRef.current !== workspaceRef.current) return;
          batch.push({ ...event.payload, kind: "reasoning" });
        }),
        listen<ToolStartEvent>("agent:tool_start", (event) => {
          const { runId, id, name, summary } = event.payload;
          batch.flush();
          routeRunEvent(runId, (tabId) => {
            commitTabs((current) =>
              current.map((tab) =>
                tab.id === tabId
                  ? {
                      ...tab,
                      activeRunId: runId,
                      items: [
                        ...settleStreaming(tab.items),
                        {
                          kind: "tool",
                          runId,
                          id,
                          name,
                          status: "pending",
                          summary,
                        },
                      ],
                    }
                  : tab,
              ),
            );
          });
        }),
        listen<ToolResultEvent>("agent:tool_result", (event) => {
          const { runId, id, ok, output } = event.payload;
          routeRunEvent(runId, (tabId) => {
            commitTabs((current) =>
              current.map((tab) =>
                tab.id === tabId
                  ? {
                      ...tab,
                      items: tab.items.map((item) =>
                        item.kind === "tool" && item.id === id
                          ? { ...item, status: ok ? "ok" : "error", output }
                          : item,
                      ),
                    }
                  : tab,
              ),
            );
          });
        }),
        listen<ApprovalEvent>("agent:approval", (event) => {
          const { runId, id, requestId, prompt, detail } = event.payload;
          routeRunEvent(runId, (tabId) => {
            commitTabs((current) =>
              current.map((tab) =>
                tab.id === tabId
                  ? {
                      ...tab,
                      items: tab.items.map((item) =>
                        item.kind === "tool" && item.id === id
                          ? {
                              ...item,
                              status: "awaiting",
                              requestId,
                              approvalPrompt: prompt,
                              approvalDetail: detail,
                            }
                          : item,
                      ),
                    }
                  : tab,
              ),
            );
          });
        }),
        listen<DoneEvent>("agent:done", (event) => {
          const { runId, messages } = event.payload;
          batch.flush();
          routeRunEvent(runId, (tabId) => {
            const tab = tabsRef.current.find(
              (candidate) => candidate.id === tabId,
            );
            if (!tab) return;
            const nextItems = settleStreaming(tab.items);
            patchTab(tabId, {
              messages,
              running: false,
              activeRunId: null,
              runId: null,
              items: nextItems,
            });
            persistTab(tabId, nextItems, messages);
            onWorkspaceRefreshRef.current();
          });
        }),
        listen<ErrorEvent>("agent:error", (event) => {
          const { runId, message } = event.payload;
          batch.flush();
          routeRunEvent(runId, (tabId) => {
            const tab = tabsRef.current.find(
              (candidate) => candidate.id === tabId,
            );
            if (!tab) return;
            const notice: Item = { kind: "notice", tone: "error", text: message };
            patchTab(tabId, {
              running: false,
              activeRunId: null,
              runId: null,
              items: [...settleStreaming(tab.items), notice],
            });
            onWorkspaceRefreshRef.current();
          });
        }),
        listen<RunIdEvent>("agent:stopped", (event) => {
          const { runId } = event.payload;
          batch.flush();
          routeRunEvent(runId, (tabId) => {
            const tab = tabsRef.current.find(
              (candidate) => candidate.id === tabId,
            );
            if (!tab) return;
            const notice: Item = { kind: "notice", tone: "muted", text: "Stopped." };
            patchTab(tabId, {
              running: false,
              activeRunId: null,
              runId: null,
              items: [...settleStreaming(tab.items), notice],
            });
            onWorkspaceRefreshRef.current();
          });
        }),
      ]);

      if (disposed) {
        subscriptions.forEach((unlisten) => unlisten());
      } else {
        unlisteners.push(...subscriptions);
      }
    };

    register().catch((error) => {
      const tab = tabsRef.current[0];
      if (tab) addNotice(tab.id, "error", errorText(error));
    });

    return () => {
      disposed = true;
      batch.clear();
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  function newTab() {
    tabCountRef.current += 1;
    const tab = blankTab(tabCountRef.current);
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
      const replacement = blankTab(tabCountRef.current);
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
    runWorkspaceRef.current = workspaceRef.current;
    pendingSendsRef.current.add(tab.id);
    commitTabs((current) =>
      current.map((candidate) =>
        candidate.id === tab.id
          ? {
              ...candidate,
              messages: nextMessages,
              items: [...candidate.items, userItem],
              running: true,
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
    const tab = tabsRef.current.find((candidate) => candidate.id === activeTabId);
    if (!tab || tab.running) return;
    const snapshot = history.find((session) => session.id === id);
    if (!snapshot) return;

    patchTab(tab.id, {
      messages: snapshot.messages,
      items: snapshot.items,
      activeRunId: null,
      runId: null,
      savedSessionId: snapshot.id,
    });
  }

  function respondToApproval(item: ToolItem, choice: ApprovalChoice) {
    if (!item.requestId) return;
    const tab = tabsRef.current.find((candidate) => candidate.id === activeTabId);
    if (!tab) return;

    const requestId = item.requestId;
    patchTab(tab.id, {
      items: tab.items.map((entry) =>
        entry.kind === "tool" && entry.id === item.id
          ? { ...entry, status: "running", requestId: undefined }
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
    tabs: tabs.map(({ id, title, running }): ChatTabInfo => ({
      id,
      title,
      running,
    })),
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
