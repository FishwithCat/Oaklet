import { settleStreaming, type Item } from "./chat-model.ts";

export type ChatTabInfo = {
  id: string;
  title: string;
  running: boolean;
  status: "working" | "idle" | "done" | "block";
};

export type ChatTabState = {
  id: string;
  title: string;
  running: boolean;
  completed: boolean;
  items: Item[];
  messages: unknown[];
  context: ContextUsage | null;
  activeRunId: string | null;
  runId: string | null;
  savedSessionId: string | null;
};

export type ContextUsage = { used: number; window: number };

export type StreamDelta = {
  kind: "assistant" | "reasoning";
  runId: string;
  text: string;
};

/** Create a tab from an id supplied by the caller so conversions stay deterministic. */
export function blankTab(id: string, number: number): ChatTabState {
  return {
    id,
    title: `Chat ${number}`,
    running: false,
    completed: false,
    items: [],
    messages: [],
    context: null,
    activeRunId: null,
    runId: null,
    savedSessionId: null,
  };
}

/** Record the size of the conversation the next request will send. */
export function applyContext(
  tab: ChatTabState,
  event: ContextUsage,
): ChatTabState {
  return { ...tab, context: { used: event.used, window: event.window } };
}

export function tabStatus(tab: ChatTabState): ChatTabInfo["status"] {
  const blocked = tab.items.some(
    (item) => item.kind === "tool" && item.status === "awaiting",
  );
  if (blocked) return "block";
  if (tab.running) return "working";
  if (tab.completed) return "done";
  return "idle";
}

export function toChatTabInfo(tab: ChatTabState): ChatTabInfo {
  return {
    id: tab.id,
    title: tab.title,
    running: tab.running,
    status: tabStatus(tab),
  };
}

/** Merge a batch of streamed deltas into one tab, settling the previous stream. */
export function applyDeltas(
  tab: ChatTabState,
  runId: string,
  deltas: readonly StreamDelta[],
): ChatTabState {
  let items = tab.items;
  for (const delta of deltas) {
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
      items = [...settleStreaming(items), { ...delta, streaming: true }];
    }
  }
  return { ...tab, items, activeRunId: runId };
}

export function applyToolStart(
  tab: ChatTabState,
  event: { runId: string; id: string; name: string; summary: string },
  startedAt: number,
): ChatTabState {
  return {
    ...tab,
    activeRunId: event.runId,
    items: [
      ...settleStreaming(tab.items),
      {
        kind: "tool",
        runId: event.runId,
        id: event.id,
        name: event.name,
        status: "pending",
        summary: event.summary,
        startedAt,
      },
    ],
  };
}

export function applyToolResult(
  tab: ChatTabState,
  event: { id: string; ok: boolean; output: string },
  finishedAt: number,
): ChatTabState {
  return {
    ...tab,
    items: tab.items.map((item) =>
      item.kind === "tool" && item.id === event.id
        ? {
            ...item,
            status: event.ok ? "ok" : "error",
            output: event.output,
            durationMs:
              item.startedAt === undefined
                ? undefined
                : finishedAt - item.startedAt,
            startedAt: undefined,
          }
        : item,
    ),
  };
}

export function applyApproval(
  tab: ChatTabState,
  event: {
    id: string;
    requestId: string;
    prompt: string;
    detail: string;
  },
): ChatTabState {
  return {
    ...tab,
    items: tab.items.map((item) =>
      item.kind === "tool" && item.id === event.id
        ? {
            ...item,
            status: "awaiting",
            requestId: event.requestId,
            approvalPrompt: event.prompt,
            approvalDetail: event.detail,
          }
        : item,
    ),
  };
}

export function finishDone(
  tab: ChatTabState,
  messages: unknown[],
): ChatTabState {
  return {
    ...tab,
    messages,
    running: false,
    completed: true,
    activeRunId: null,
    runId: null,
    items: settleStreaming(tab.items),
  };
}

export function finishError(tab: ChatTabState, message: string): ChatTabState {
  return {
    ...tab,
    running: false,
    completed: false,
    activeRunId: null,
    runId: null,
    items: [
      ...settleStreaming(tab.items),
      { kind: "notice", tone: "error", text: message },
    ],
  };
}

export function finishStopped(tab: ChatTabState): ChatTabState {
  return {
    ...tab,
    running: false,
    completed: false,
    activeRunId: null,
    runId: null,
    items: [
      ...settleStreaming(tab.items),
      { kind: "notice", tone: "muted", text: "Stopped." },
    ],
  };
}
