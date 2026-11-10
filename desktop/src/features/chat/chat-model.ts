import type { ProcessItem } from "./process-model";

export type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | ProcessItem
  | { kind: "notice"; text: string; tone: "error" | "muted" };

/**
 * Put a generated request into the composer without discarding a draft the
 * user is already writing. Re-appending the same prompt is a no-op.
 */
export function mergeDraft(current: string, prompt: string): string {
  const draft = current.trimEnd();
  const addition = prompt.trim();
  if (!addition) return current;
  if (!draft) return addition;
  if (draft.includes(addition)) return draft;
  return `${draft}\n\n${addition}`;
}

export function settleStreaming(items: Item[]): Item[] {
  return items.map((item) =>
    (item.kind === "assistant" || item.kind === "reasoning") && item.streaming
      ? { ...item, streaming: false }
      : item,
  );
}

/** Share of the context window in use: 1600/262144 -> "0.6%". */
export function formatContextPercent(used: number, window: number): string {
  if (used <= 0 || window <= 0) return "0%";
  const percent = (used / window) * 100;
  if (percent < 0.1) return "<0.1%";
  const rounded = Math.round(percent * 10) / 10;
  return `${rounded < 10 ? rounded.toFixed(1) : Math.round(rounded)}%`;
}

/** Compact token counts for the gauge tooltip: 12400 -> "12k". */
export function formatTokens(tokens: number): string {
  if (tokens < 1024) return `${tokens}`;
  const thousands = tokens / 1024;
  return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
}

/** Hover label for the context bar, e.g. "30% (77k/256k)". */
export function formatContextUsage(used: number, window: number): string {
  return `${formatContextPercent(used, window)} (${formatTokens(used)}/${formatTokens(window)})`;
}

export type ContextTier = "normal" | "elevated" | "high" | "critical";

/** Plant color for the context gauge, stepping up at 30%, 60%, and 80%. */
export function contextTier(used: number, window: number): ContextTier {
  const ratio = window > 0 ? used / window : 0;
  if (ratio >= 0.8) return "critical";
  if (ratio >= 0.6) return "high";
  if (ratio >= 0.3) return "elevated";
  return "normal";
}

/**
 * Keep the plant visible for a conversation whose usage predates the gauge,
 * such as a session restored from history without a saved context snapshot.
 */
export function shouldShowContextGauge(
  context: { used: number; window: number } | null,
  hasConversation: boolean,
): boolean {
  return context !== null || hasConversation;
}

/** A tab the user has not used yet; restoring a session can replace it. */
export function isBlankTab(tab: {
  running: boolean;
  items: Item[];
  savedSessionId: string | null;
}): boolean {
  return !tab.running && tab.items.length === 0 && !tab.savedSessionId;
}

export type Block =
  | { kind: "process"; start: number; items: ProcessItem[] }
  | { kind: "single"; index: number; item: Item };

/** Reasoning and tool rows between replies become one compact process group. */
export function toBlocks(items: Item[]): Block[] {
  const blocks: Block[] = [];
  let process: ProcessItem[] | null = null;
  let start = 0;

  items.forEach((item, index) => {
    if (item.kind === "reasoning" || item.kind === "tool") {
      if (process === null) {
        process = [];
        start = index;
      }
      process.push(item);
      return;
    }
    if (process !== null) {
      blocks.push({ kind: "process", start, items: process });
      process = null;
    }
    blocks.push({ kind: "single", index, item });
  });

  if (process !== null) {
    blocks.push({ kind: "process", start, items: process });
  }
  return blocks;
}
