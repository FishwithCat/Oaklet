import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { createStreamBatch } from "./stream-batch";
import { isBlankTab, type Item } from "./chat-model";
import { sentAttachments, type Attachment } from "./attachments";
import {
  appendNotice,
  accumulateUsage,
  applyApproval,
  applyApprovalResolved,
  applyContext,
  applyDeltas,
  applyToolResult,
  applyToolStart,
  mergeWindow,
  prependWindow,
  settleRun,
  toChatTabInfo,
  totalUsage,
  withItems,
  type SessionSummary,
  type SessionView,
  type RunUsage,
  type StreamDelta,
  type TabUsage,
} from "./session-model.ts";
import { subscribeAgentEvents } from "./agent-events.ts";
import {
  pickStartupSession,
  readLastSessionId,
  rememberLastSession,
} from "./session-restore.ts";

type OpenTab = { id: string; number: number };

type SendResult = { runId: string; session: SessionView };

type StartupStatus = "loading" | "ready" | "error";

type CompactResult = {
  session: SessionView;
  before: number;
  after: number;
  compacted: boolean;
};

/**
 * Chat tabs backed by Rust. The backend owns sessions, runs, and the transcript;
 * this hook keeps a render cache hydrated from command responses and events.
 * Open tabs are client UI state; only the selected saved session id is kept in
 * localStorage so the next launch can reopen it.
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
  const [compactingIds, setCompactingIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [loadOlderError, setLoadOlderError] = useState<{
    id: string;
    text: string;
  } | null>(null);
  const [usageBySession, setUsageBySession] = useState<Record<string, TabUsage>>({});
  const [startupStatus, setStartupStatus] =
    useState<StartupStatus>("loading");
  const [startupError, setStartupError] = useState("");

  const sessionsRef = useRef(sessions);
  const openTabsRef = useRef(openTabs);
  const activeTabIdRef = useRef(activeTabId);
  const workspaceRef = useRef(workspace);
  const tabCountRef = useRef(0);
  const compactingIdsRef = useRef(new Set<string>());
  const loadingOlderRef = useRef(false);
  const startedForRef = useRef<string | null>(null);
  const onWorkspaceRefreshRef = useRef(onWorkspaceRefresh);
  const usageBySessionRef = useRef(usageBySession);
  const startupTokenRef = useRef(0);
  const creatingTabRef = useRef(false);

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

  function recordUsage(id: string, runId: string, usage: RunUsage) {
    const next = {
      ...usageBySessionRef.current,
      [id]: accumulateUsage(usageBySessionRef.current[id], runId, usage),
    };
    usageBySessionRef.current = next;
    setUsageBySession(next);
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
    usageBySessionRef.current = {};
    setUsageBySession({});
  }

  /** Keep the visible saved session as the next startup target. */
  function rememberSession(id: string, workspace = workspaceRef.current) {
    const tab = sessionsRef.current[id];
    if (tab && tab.turns > 0) rememberLastSession(workspace, id);
  }

  /** A deliberate user action wins over any startup response still in flight. */
  function cancelStartup() {
    startupTokenRef.current += 1;
    setStartupError("");
    setStartupStatus("ready");
  }

  function addTab(view: SessionView) {
    tabCountRef.current += 1;
    commitTabs([...openTabsRef.current, { id: view.id, number: tabCountRef.current }]);
    putSession(view);
    activate(view.id);
    rememberSession(view.id);
  }

  /**
   * Open the remembered session when it is still in this workspace's history,
   * otherwise the newest one. A workspace with no history gets a blank chat.
   * Every await is checked against the workspace and the startup token so an
   * old response cannot overwrite a tab the user opened in the meantime.
   */
  function runStartup(workspace: string) {
    const token = ++startupTokenRef.current;
    setStartupError("");
    setStartupStatus("loading");
    const current = () =>
      workspaceRef.current === workspace && startupTokenRef.current === token;

    void (async () => {
      try {
        const list = await invoke<SessionSummary[]>("session_list");
        if (!current()) return;
        setHistory(list);
        if (openTabsRef.current.length > 0) {
          setStartupStatus("ready");
          return;
        }

        const picked = pickStartupSession(
          list,
          readLastSessionId(workspace),
        );
        const view = picked
          ? await invoke<SessionView>("session_get", { id: picked.id })
          : await invoke<SessionView>("session_create");
        if (!current()) return;
        if (openTabsRef.current.length > 0) {
          setStartupStatus("ready");
          return;
        }
        addTab(view);
        setStartupStatus("ready");
      } catch (error) {
        if (!current()) return;
        setStartupError(errorText(error));
        setStartupStatus("error");
      }
    })();
  }

  function retryStartup() {
    const workspace = workspaceRef.current;
    if (workspace) runStartup(workspace);
  }

  function showError(text: string) {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (tab) updateSession(tab.id, (view) => appendNotice(view, "error", text));
    else console.error(text);
  }

  function refreshHistory() {
    const workspace = workspaceRef.current;
    if (!workspace) return;
    invoke<SessionSummary[]>("session_list")
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
    runStartup(workspace);
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
      putSession(session ? mergeWindow(tab, session) : settleRun(tab, notice));
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
      onUsage: (event) => {
        if (disposed) return;
        recordUsage(event.sessionId, event.runId, {
          promptTokens: event.promptTokens,
          completionTokens: event.completionTokens,
          cacheHitTokens: event.cacheHitTokens,
          cacheMissTokens: event.cacheMissTokens,
          steps: event.steps,
        });
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
    if (creatingTabRef.current) return;

    const firstTab = openTabsRef.current.length === 0;
    cancelStartup();
    if (firstTab) setStartupStatus("loading");
    creatingTabRef.current = true;
    try {
      const view = await invoke<SessionView>("session_create");
      if (workspaceRef.current !== workspace) return;
      addTab(view);
      setStartupStatus("ready");
    } catch (error) {
      if (workspaceRef.current !== workspace) return;
      if (sessionsRef.current[activeTabIdRef.current]) {
        showError(errorText(error));
      } else {
        setStartupError(errorText(error));
        setStartupStatus("error");
      }
    } finally {
      creatingTabRef.current = false;
    }
  }

  function selectTab(id: string) {
    if (openTabsRef.current.some((tab) => tab.id === id)) {
      activate(id);
      rememberSession(id);
    }
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
      const nextId = (remaining[index] ?? remaining[index - 1])!.id;
      activate(nextId);
      rememberSession(nextId);
    }
  }

  async function send(
    text: string,
    attachments: Attachment[] = [],
  ): Promise<boolean> {
    const trimmed = text.trim();
    if ((!trimmed && attachments.length === 0) || !enabled) return false;

    const tab = sessionsRef.current[activeTabIdRef.current];
    if (
      !tab ||
      tab.running ||
      tab.compacting ||
      compactingIdsRef.current.has(tab.id)
    ) {
      return false;
    }

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

    try {
      const { runId, session } = await invoke<SendResult>("session_send", {
        sessionId: tab.id,
        text: trimmed,
        attachments,
      });
      if (workspaceRef.current !== workspaceAtSend) return false;
      if (
        session.turns > 0 &&
        sessionsRef.current[tab.id] &&
        activeTabIdRef.current === tab.id
      ) {
        rememberLastSession(workspaceAtSend, tab.id);
      }
      const current = sessionsRef.current[tab.id];
      // A terminal event may already have replaced the optimistic view.
      if (!current || !current.running) return true;
      // The run finished before the command response; its view is final.
      if (!session.running) {
        putSession(mergeWindow(current, session));
        return true;
      }
      putSession({
        ...current,
        title: session.title,
        turns: session.turns,
        updatedAt: session.updatedAt,
        running: true,
        activeRunId: runId,
      });
      return true;
    } catch (error) {
      if (workspaceRef.current !== workspaceAtSend) return false;
      if (!sessionsRef.current[tab.id]) return false;
      putSession(appendNotice(previous, "error", errorText(error)));
      return false;
    }
  }

  function stop() {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (tab?.running) void invoke("session_stop", { sessionId: tab.id });
  }

  /** Start the implementation run bound to one validated plan version. */
  async function confirmPlan(version: number): Promise<boolean> {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (
      !tab ||
      tab.running ||
      tab.compacting ||
      compactingIdsRef.current.has(tab.id)
    ) {
      return false;
    }
    const workspaceAtStart = workspaceRef.current;
    const previous = tab;
    putSession({ ...withItems(tab, tab.items), running: true, activeRunId: null });
    try {
      const { runId, session } = await invoke<SendResult>(
        "session_confirm_plan",
        { sessionId: tab.id, version },
      );
      if (workspaceRef.current !== workspaceAtStart) return false;
      const current = sessionsRef.current[tab.id];
      if (!current || !current.running) return true;
      if (!session.running) {
        putSession(mergeWindow(current, session));
        return true;
      }
      putSession({
        ...current,
        title: session.title,
        turns: session.turns,
        updatedAt: session.updatedAt,
        running: true,
        activeRunId: runId,
      });
      return true;
    } catch (error) {
      if (workspaceRef.current !== workspaceAtStart) return false;
      if (!sessionsRef.current[tab.id]) return false;
      putSession(appendNotice(previous, "error", errorText(error)));
      return false;
    }
  }

  function compact() {
    const tab = sessionsRef.current[activeTabIdRef.current];
    if (!tab) return;
    if (compactingIdsRef.current.has(tab.id) || tab.compacting) {
      void invoke("session_compact_cancel", { sessionId: tab.id });
      return;
    }
    if (
      tab.running ||
      loadingOlderRef.current ||
      tab.turns === 0
    ) {
      return;
    }

    compactingIdsRef.current.add(tab.id);
    setCompactingIds(new Set(compactingIdsRef.current));
    const workspaceAtStart = workspaceRef.current;
    invoke<CompactResult>("session_compact", { sessionId: tab.id })
      .then(({ session, compacted }) => {
        if (workspaceRef.current !== workspaceAtStart) return;
        const current = sessionsRef.current[tab.id];
        if (!current) return;
        putSession(mergeWindow(current, session));
        if (!compacted) {
          updateSession(tab.id, (view) =>
            appendNotice(view, "muted", "Nothing older to compact yet."),
          );
        }
      })
      .catch((error) => {
        if (workspaceRef.current === workspaceAtStart) {
          const message = errorText(error);
          updateSession(tab.id, (view) =>
            appendNotice(
              view,
              message === "Stopped." ? "muted" : "error",
              message === "Stopped." ? "Compaction stopped." : message,
            ),
          );
        }
      })
      .finally(() => {
        compactingIdsRef.current.delete(tab.id);
        setCompactingIds(new Set(compactingIdsRef.current));
      });
  }

  async function restore(id: string) {
    if (openTabsRef.current.some((tab) => tab.id === id)) {
      activate(id);
      rememberSession(id);
      return;
    }

    // A manual history pick supersedes a startup restore that is still loading.
    cancelStartup();
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
      if (workspaceRef.current !== workspaceAtStart) return;
      if (sessionsRef.current[activeTabIdRef.current]) {
        showError(errorText(error));
      } else {
        setStartupError(errorText(error));
        setStartupStatus("error");
      }
    }
  }

  /** Pull one older transcript page and prepend it to the open tab. */
  async function loadOlder(): Promise<boolean> {
    const id = activeTabIdRef.current;
    const tab = sessionsRef.current[id];
    if (
      !tab ||
      !tab.hasMore ||
      tab.firstSeq === null ||
      loadingOlderRef.current ||
      tab.compacting ||
      compactingIdsRef.current.has(id)
    ) {
      return false;
    }
    const workspaceAtStart = workspaceRef.current;
    setLoadOlderError(null);
    loadingOlderRef.current = true;
    setLoadingOlder(true);
    try {
      const older = await invoke<SessionView>("session_get", {
        id,
        beforeSeq: tab.firstSeq,
      });
      if (workspaceRef.current !== workspaceAtStart) return false;
      const current = sessionsRef.current[id];
      if (
        !current ||
        older.firstSeq === null ||
        (current.firstSeq !== null && older.firstSeq >= current.firstSeq)
      ) {
        return false;
      }
      const next = prependWindow(current, older);
      if (next === current) return false;
      putSession(next);
      setLoadOlderError(null);
      return true;
    } catch (error) {
      if (workspaceRef.current === workspaceAtStart) {
        setLoadOlderError({ id, text: errorText(error) });
      }
      return false;
    } finally {
      loadingOlderRef.current = false;
      setLoadingOlder(false);
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
          entry.kind === "tool" && entry.runId === item.runId && entry.id === item.id
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

  function grantAllowAll() {
    invoke("grant_command_allow_all")
      .then(() => setAllowAll(true))
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
  const activeUsage = usageBySession[resolvedActiveId];
  const busy = openTabs.some(({ id }) => {
    const view = sessions[id];
    return Boolean(
      view?.running || view?.compacting || compactingIds.has(id),
    );
  });

  return {
    items: activeTab?.items ?? [],
    firstSeq: activeTab?.firstSeq ?? null,
    hasMore: activeTab?.hasMore ?? false,
    loadingOlder,
    loadOlderError:
      loadOlderError?.id === resolvedActiveId ? loadOlderError.text : "",
    running: activeTab?.running ?? false,
    activeRunId: activeTab?.activeRunId ?? null,
    compacting:
      compactingIds.has(resolvedActiveId) || (activeTab?.compacting ?? false),
    tabs,
    activeTabId: resolvedActiveId,
    context: activeTab?.context ?? null,
    usage: activeUsage ? totalUsage(activeUsage) : null,
    stepUsage: activeUsage?.lastStep ?? activeTab?.lastUsage ?? null,
    busy,
    allowAll,
    history,
    restoring: startupStatus === "loading",
    restoreError: startupStatus === "error" ? startupError : "",
    retryRestore: retryStartup,
    newTab,
    selectTab,
    closeTab,
    send,
    confirmPlan,
    stop,
    compact,
    restore,
    loadOlder,
    respondToApproval,
    revokeAllowAll,
    grantAllowAll,
    reloadAllowance: loadAllowance,
    refreshHistory,
  };
}
