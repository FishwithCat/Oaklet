import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { settleStreaming, type Item } from "./chat-model";

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

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}

export function useAgentSession({
  enabled,
  onWorkspaceRefresh,
}: {
  enabled: boolean;
  onWorkspaceRefresh: () => void;
}) {
  const [items, setItems] = useState<Item[]>([]);
  const [running, setRunning] = useState(false);
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [allowAll, setAllowAll] = useState(false);

  const messagesRef = useRef<unknown[]>([]);
  const runIdRef = useRef<string | null>(null);
  const onWorkspaceRefreshRef = useRef(onWorkspaceRefresh);

  useEffect(() => {
    onWorkspaceRefreshRef.current = onWorkspaceRefresh;
  }, [onWorkspaceRefresh]);

  function addNotice(tone: "error" | "muted", text: string) {
    setItems((previous) => [...previous, { kind: "notice", tone, text }]);
  }

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

    const register = async () => {
      const subscriptions = await Promise.all([
        listen<DeltaEvent>("agent:delta", (event) => {
          const { runId, text } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => {
            const last = previous[previous.length - 1];
            if (last?.kind === "assistant" && last.streaming) {
              return [
                ...previous.slice(0, -1),
                { ...last, text: last.text + text },
              ];
            }
            return [
              ...settleStreaming(previous),
              { kind: "assistant", text, streaming: true },
            ];
          });
        }),
        listen<ReasoningEvent>("agent:reasoning", (event) => {
          const { runId, text } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => {
            const last = previous[previous.length - 1];
            if (
              last?.kind === "reasoning" &&
              last.streaming &&
              last.runId === runId
            ) {
              return [
                ...previous.slice(0, -1),
                { ...last, text: last.text + text },
              ];
            }
            return [
              ...settleStreaming(previous),
              { kind: "reasoning", runId, text, streaming: true },
            ];
          });
        }),
        listen<ToolStartEvent>("agent:tool_start", (event) => {
          const { runId, id, name, summary } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => [
            ...settleStreaming(previous),
            {
              kind: "tool",
              runId,
              id,
              name,
              status: "pending",
              summary,
            },
          ]);
        }),
        listen<ToolResultEvent>("agent:tool_result", (event) => {
          const { id, ok, output } = event.payload;
          setItems((previous) =>
            previous.map((item) =>
              item.kind === "tool" && item.id === id
                ? { ...item, status: ok ? "ok" : "error", output }
                : item,
            ),
          );
        }),
        listen<ApprovalEvent>("agent:approval", (event) => {
          const { id, requestId, prompt, detail } = event.payload;
          setItems((previous) =>
            previous.map((item) =>
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
          );
        }),
        listen<DoneEvent>("agent:done", (event) => {
          messagesRef.current = event.payload.messages;
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          setItems(settleStreaming);
          onWorkspaceRefreshRef.current();
        }),
        listen<ErrorEvent>("agent:error", (event) => {
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          onWorkspaceRefreshRef.current();
          setItems((previous) => [
            ...settleStreaming(previous),
            { kind: "notice", tone: "error", text: event.payload.message },
          ]);
        }),
        listen<RunIdEvent>("agent:stopped", () => {
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          onWorkspaceRefreshRef.current();
          setItems((previous) => [
            ...settleStreaming(previous),
            { kind: "notice", tone: "muted", text: "Stopped." },
          ]);
        }),
      ]);

      if (disposed) {
        subscriptions.forEach((unlisten) => unlisten());
      } else {
        unlisteners.push(...subscriptions);
      }
    };

    register().catch((error) => {
      setItems((previous) => [
        ...previous,
        { kind: "notice", tone: "error", text: errorText(error) },
      ]);
    });

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  function send(text: string): boolean {
    const trimmed = text.trim();
    if (!trimmed || running || !enabled) return false;

    const nextMessages = [
      ...messagesRef.current,
      { role: "user", content: trimmed },
    ];
    messagesRef.current = nextMessages;
    setItems((previous) => [...previous, { kind: "user", text: trimmed }]);
    setRunning(true);
    setActiveRunId(null);

    invoke<string>("agent_send", { messages: nextMessages })
      .then((runId) => {
        runIdRef.current = runId;
      })
      .catch((error) => {
        runIdRef.current = null;
        setRunning(false);
        addNotice("error", errorText(error));
      });
    return true;
  }

  function stop() {
    if (runIdRef.current) {
      void invoke("agent_stop", { runId: runIdRef.current });
    }
  }

  function reset() {
    if (running) return;
    messagesRef.current = [];
    runIdRef.current = null;
    setActiveRunId(null);
    setItems([]);
  }

  function respondToApproval(item: ToolItem, choice: ApprovalChoice) {
    if (!item.requestId) return;
    const requestId = item.requestId;
    setItems((previous) =>
      previous.map((entry) =>
        entry.kind === "tool" && entry.id === item.id
          ? { ...entry, status: "running", requestId: undefined }
          : entry,
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
        addNotice("error", errorText(error));
      });
  }

  function revokeAllowAll() {
    invoke("revoke_command_allow_all")
      .then(() => setAllowAll(false))
      .catch((error) => {
        addNotice("error", errorText(error));
      });
  }

  return {
    items,
    running,
    activeRunId,
    allowAll,
    send,
    stop,
    reset,
    respondToApproval,
    revokeAllowAll,
    reloadAllowance: loadAllowance,
  };
}
