import { settleStreaming, type Item } from "./chat-model.ts";
import type { FileDiff } from "./process-model.ts";

export type ContextUsage = { used: number; window: number };

/** Authoritative session state from Rust. Items are the rendered transcript. */
export type SessionView = {
  id: string;
  title: string;
  updatedAt: number;
  turns: number;
  items: Item[];
  /** Seq of the oldest event in this window, or null for an empty session. */
  firstSeq: number | null;
  /** Whether events exist before `firstSeq`. */
  hasMore: boolean;
  context: ContextUsage | null;
  running: boolean;
  blocked: boolean;
  compacting: boolean;
  activeRunId: string | null;
};

export type SessionSummary = {
  id: string;
  title: string;
  updatedAt: number;
  turns: number;
  running: boolean;
  blocked: boolean;
  compacting: boolean;
};

export type ChatTabInfo = {
  id: string;
  title: string;
  running: boolean;
  status: "working" | "idle" | "done" | "block";
};

export type StreamDelta = {
  kind: "assistant" | "reasoning";
  sessionId: string;
  runId: string;
  text: string;
};

/** Replace items and keep the blocked flag consistent with them. */
export function withItems(tab: SessionView, items: Item[]): SessionView {
  return {
    ...tab,
    items,
    blocked: items.some(
      (item) => item.kind === "tool" && item.status === "awaiting",
    ),
  };
}

/**
 * Merge a newer window into the items already on screen. Items before the
 * incoming window (older pages the user loaded) stay; items at or after it are
 * replaced by the fresh projection. Local streaming rows have no seq and are
 * dropped once the persisted window supplies their settled form.
 */
export function mergeWindow(
  current: SessionView,
  incoming: SessionView,
): SessionView {
  const first = incoming.firstSeq;
  const items =
    first === null
      ? current.items
      : [
          ...current.items.filter(
            (item) => item.seq !== undefined && item.seq < first,
          ),
          ...incoming.items,
        ];
  const firstSeq =
    current.firstSeq === null
      ? first
      : first === null
        ? current.firstSeq
        : Math.min(current.firstSeq, first);
  const hasMore =
    first === null
      ? current.hasMore
      : current.firstSeq !== null && current.firstSeq < first
        ? current.hasMore
        : incoming.hasMore;
  return { ...withItems(incoming, items), firstSeq, hasMore };
}

/** Prepend an older page; returns the same view when it is not older. */
export function prependWindow(
  current: SessionView,
  older: SessionView,
): SessionView {
  if (
    older.firstSeq === null ||
    (current.firstSeq !== null && older.firstSeq >= current.firstSeq)
  ) {
    return current;
  }
  const boundary = current.firstSeq;
  const olderItems =
    boundary === null
      ? older.items
      : older.items.filter(
          (item) => item.seq === undefined || item.seq < boundary,
        );
  if (olderItems.length === 0) {
    return {
      ...current,
      firstSeq: older.firstSeq,
      hasMore: older.hasMore,
    };
  }
  return {
    ...current,
    items: [...olderItems, ...current.items],
    firstSeq: older.firstSeq,
    hasMore: older.hasMore,
  };
}

export function applyContext(
  tab: SessionView,
  event: ContextUsage,
): SessionView {
  return { ...tab, context: { used: event.used, window: event.window } };
}

export function tabStatus(tab: SessionView): ChatTabInfo["status"] {
  const blocked = tab.items.some(
    (item) => item.kind === "tool" && item.status === "awaiting",
  );
  if (blocked) return "block";
  if (tab.running) return "working";
  if (tab.turns > 0) return "done";
  return "idle";
}

export function toChatTabInfo(tab: SessionView, number: number): ChatTabInfo {
  return {
    id: tab.id,
    title: tab.title || `Chat ${number}`,
    running: tab.running,
    status: tabStatus(tab),
  };
}

/** Merge a batch of streamed deltas into one session. */
export function applyDeltas(
  tab: SessionView,
  runId: string,
  deltas: readonly StreamDelta[],
): SessionView {
  let items = tab.items;
  for (const delta of deltas) {
    const last = items[items.length - 1];
    if (
      last &&
      last.kind === delta.kind &&
      (last.kind === "assistant" || last.kind === "reasoning") &&
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
        {
          kind: delta.kind,
          runId: delta.runId,
          text: delta.text,
          streaming: true,
        },
      ];
    }
  }
  return { ...withItems(tab, items), running: true, activeRunId: runId };
}

export function applyToolStart(
  tab: SessionView,
  event: {
    runId: string;
    id: string;
    name: string;
    summary: string;
    diffs?: FileDiff[];
  },
  startedAt: number,
): SessionView {
  const items = [
    ...settleStreaming(tab.items),
    {
      kind: "tool" as const,
      runId: event.runId,
      id: event.id,
      name: event.name,
      status: "pending" as const,
      summary: event.summary,
      startedAt,
      ...(event.diffs && event.diffs.length > 0
        ? { diffs: event.diffs }
        : {}),
    },
  ];
  return {
    ...withItems(tab, items),
    running: true,
    activeRunId: event.runId,
  };
}

export function applyToolResult(
  tab: SessionView,
  event: { id: string; ok: boolean; output: string; diffs?: FileDiff[] },
  finishedAt: number,
): SessionView {
  const items = tab.items.map((item) => {
    if (item.kind !== "tool" || item.id !== event.id) return item;
    const next = {
      ...item,
      status: event.ok ? ("ok" as const) : ("error" as const),
      output: event.output,
      durationMs:
        item.startedAt === undefined ? undefined : finishedAt - item.startedAt,
      startedAt: undefined,
    };
    if (event.diffs && event.diffs.length > 0) next.diffs = event.diffs;
    else delete next.diffs;
    return next;
  });
  return withItems(tab, items);
}

export function applyApproval(
  tab: SessionView,
  event: {
    id: string;
    requestId: string;
    prompt: string;
    detail: string;
  },
): SessionView {
  const items = tab.items.map((item) =>
    item.kind === "tool" && item.id === event.id
      ? {
          ...item,
          status: "awaiting" as const,
          requestId: event.requestId,
          approvalPrompt: event.prompt,
          approvalDetail: event.detail,
        }
      : item,
  );
  return withItems(tab, items);
}

export function applyApprovalResolved(
  tab: SessionView,
  event: { id: string; allow: boolean },
  now: number,
): SessionView {
  const items = tab.items.map((item) =>
    item.kind === "tool" && item.id === event.id
      ? {
          ...item,
          status: "running" as const,
          requestId: undefined,
          approvalPrompt: undefined,
          approvalDetail: undefined,
          startedAt: event.allow ? now : undefined,
        }
      : item,
  );
  return withItems(tab, items);
}

export function appendNotice(
  tab: SessionView,
  tone: "error" | "muted",
  text: string,
): SessionView {
  return withItems(tab, [...tab.items, { kind: "notice", tone, text }]);
}

/** Settle a run whose terminal event could not include a persisted session. */
export function settleRun(
  tab: SessionView,
  notice?: { tone: "error" | "muted"; text: string },
): SessionView {
  const items = settleStreaming(
    tab.items.map((item) =>
      item.kind === "tool" &&
      (item.status === "pending" ||
        item.status === "awaiting" ||
        item.status === "running")
        ? {
            ...item,
            status: "error" as const,
            requestId: undefined,
            startedAt: undefined,
            approvalPrompt: undefined,
            approvalDetail: undefined,
            diffs: undefined,
          }
        : item,
    ),
  );
  return {
    ...withItems(
      tab,
      notice ? [...items, { kind: "notice", ...notice }] : items,
    ),
    running: false,
    activeRunId: null,
    blocked: false,
  };
}
