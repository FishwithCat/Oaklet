import type { ProcessItem } from "./process-model";

export type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | ProcessItem
  | { kind: "notice"; text: string; tone: "error" | "muted" };

export function settleStreaming(items: Item[]): Item[] {
  return items.map((item) =>
    (item.kind === "assistant" || item.kind === "reasoning") && item.streaming
      ? { ...item, streaming: false }
      : item,
  );
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
