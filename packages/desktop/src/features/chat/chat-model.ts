import type { ProcessItem } from "./process-model";
import type { SentAttachment } from "./attachments";
import type { PlanItem } from "./plan-model";
import type { RunUsage, StepUsage } from "./session-model";

export type Item = (
  | { kind: "user"; text: string; attachments?: SentAttachment[] }
  | { kind: "assistant"; runId?: string; text: string; streaming: boolean }
  | ProcessItem
  | PlanItem
  | { kind: "notice"; text: string; tone: "error" | "muted" }
) & { seq?: number };

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

export function settleStreaming(items: Item[], now = Date.now()): Item[] {
  return items.map((item) =>
    (item.kind === "assistant" || item.kind === "reasoning") && item.streaming
      ? {
          ...item,
          streaming: false,
          ...(item.kind === "reasoning" && item.startedAt !== undefined
            ? { durationMs: Math.max(0, now - item.startedAt), startedAt: undefined }
            : {}),
        }
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
  if (thousands < 1024) {
    return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
  }
  const millions = thousands / 1024;
  return `${millions < 10 ? millions.toFixed(1) : Math.round(millions)}M`;
}

/** Share of provider prompt tokens served from cache: 1000/1234 -> "81%". */
export function formatCacheHitRate(usage: StepUsage): string {
  const total = usage.cacheHitTokens + usage.cacheMissTokens;
  if (total <= 0) return "0%";
  return `${Math.round((usage.cacheHitTokens / total) * 100)}%`;
}

/** Compact provider token counts in decimal units: 1_218_000 -> "1.2M". */
export function formatUsageTokens(tokens: number): string {
  if (tokens < 1000) return `${tokens}`;
  if (tokens < 1_000_000) {
    const thousands = tokens / 1000;
    return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
  }
  const millions = tokens / 1_000_000;
  return `${millions < 10 ? millions.toFixed(1) : Math.round(millions)}M`;
}

/** Eased token total for the composer count-up; progress is clamped to 0..1. */
export function countUpValue(from: number, to: number, progress: number): number {
  const remaining = 1 - Math.min(1, Math.max(0, progress));
  return Math.round(to - (to - from) * remaining ** 3);
}

/** Tooltip explains what the total covers: model steps plus the token split. */
export function formatUsageTitle(usage: RunUsage): string {
  const steps = `${usage.steps} ${usage.steps === 1 ? "step" : "steps"}`;
  return [
    `${steps} · cache ${formatCacheHitRate(usage)}`,
    `${formatUsageTokens(usage.cacheHitTokens)}/${formatUsageTokens(usage.promptTokens)} prompt`,
    `${formatUsageTokens(usage.completionTokens)} completion`,
  ].join(" · ");
}

/** Hover label for the context bar, e.g. "30% (77k/256k)". */
export function formatContextUsage(used: number, window: number): string {
  return `${formatContextPercent(used, window)} (${formatTokens(used)}/${formatTokens(window)})`;
}

export function formatContextLabel(used: number, window: number): string {
  return `${formatTokens(used)}/${formatTokens(window)}`;
}

/** Hover detail for the label: how the last model request was billed. */
export function formatRequestTitle(step: StepUsage): string {
  return [
    "last request",
    `cache ${formatCacheHitRate(step)}`,
    `${formatUsageTokens(step.cacheHitTokens)}/${formatUsageTokens(step.promptTokens)} prompt`,
    `${formatUsageTokens(step.completionTokens)} completion`,
  ].join(" · ");
}

/** Show the workspace relative to the home directory: ~/Workspace/Acorn. */
export function formatWorkdir(workspace: string, home: string): string {
  const separator = workspace.includes("\\") ? "\\" : "/";
  if (home && workspace === home) return "~";
  return home && workspace.startsWith(home + separator)
    ? `~/${workspace.slice(home.length + 1)}`
    : workspace;
}

/** Drop a provider prefix from a model id: "zai-org/GLM-5.3-Flash" -> "GLM-5.3-Flash". */
export function formatModelId(model: string): string {
  const id = model.trim();
  const slash = id.lastIndexOf("/");
  return slash === -1 || slash === id.length - 1 ? id : id.slice(slash + 1);
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
}): boolean {
  return !tab.running && tab.items.length === 0;
}

/** Sub-pixel slack for "the thread is scrolled all the way down". */
const BOTTOM_SLACK = 2;

/**
 * Whether a scroll container sits at its true bottom. The tolerance is tiny on
 * purpose: a wide band re-attaches after the next streamed chunk and fights an
 * upward scroll instead of letting go.
 */
export function isAtBottom(element: {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}): boolean {
  return (
    element.scrollHeight - element.scrollTop - element.clientHeight <=
    BOTTOM_SLACK
  );
}

/**
 * Whether the thread should keep following after a scroll event. Only moving
 * up and away from the bottom detaches it: the pin, streamed growth, and
 * shrinking content all fire scroll events, and treating those as "the user
 * scrolled away" is what stopped the follow after a stop.
 */
export function stickAfterScroll(
  stick: boolean,
  atBottom: boolean,
  top: number,
  previousTop: number,
): boolean {
  if (atBottom) return true;
  return top < previousTop - 1 ? false : stick;
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
