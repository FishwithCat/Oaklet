export type ToolStatus = "pending" | "awaiting" | "running" | "ok" | "error";

export type ApprovalChoice = "deny" | "allow" | "allowAll";

export type ReasoningItem = {
  kind: "reasoning";
  runId: string;
  text: string;
  streaming: boolean;
};

export type ToolItem = {
  kind: "tool";
  runId: string;
  id: string;
  name: string;
  status: ToolStatus;
  output?: string;
  /** Epoch ms when execution started, cleared once the duration is known. */
  startedAt?: number;
  /** Execution time reported when the tool finishes. */
  durationMs?: number;
  requestId?: string;
  summary?: string;
  approvalPrompt?: string;
  approvalDetail?: string;
};

export type ProcessItem = ReasoningItem | ToolItem;

export type ToolCategory = "read" | "write" | "edit" | "command" | "tools";

const TOOL_CATEGORIES: Record<string, ToolCategory> = {
  read_file: "read",
  list_dir: "read",
  write_file: "write",
  edit_file: "edit",
  apply_patch: "edit",
  run_command: "command",
};

const CATEGORY_LABELS: Record<ToolCategory, { running: string; done: string }> = {
  read: { running: "Reading files", done: "Read files" },
  write: { running: "Writing files", done: "Wrote files" },
  edit: { running: "Editing files", done: "Edited files" },
  command: { running: "Running commands", done: "Ran commands" },
  tools: { running: "Calling tools", done: "Called tools" },
};

const TOOL_TITLES: Record<string, string> = {
  read_file: "Read file",
  list_dir: "List folder",
  write_file: "Write file",
  edit_file: "Edit file",
  apply_patch: "Apply patch",
  run_command: "Run command",
  skill: "Load skill",
  likec4_mcp: "LikeC4 MCP",
};

export function toolCategory(name: string): ToolCategory {
  return TOOL_CATEGORIES[name] ?? "tools";
}

export function toolTitle(name: string): string {
  const known = TOOL_TITLES[name];
  if (known) return known;
  if (name.startsWith("mcp__")) {
    const separator = name.indexOf("__", 5);
    if (separator > 5) {
      return `${name.slice(5, separator)} · ${name.slice(separator + 2)}`;
    }
  }
  return name.replace(/_/g, " ");
}

/** Human-readable elapsed time, e.g. `0.4s`, `12s`, `1m 5s`. */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, ms) / 1000;
  const total = Math.round(seconds);
  if (total < 10) return `${seconds.toFixed(1)}s`;
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  const rest = total % 60;
  return rest === 0 ? `${minutes}m` : `${minutes}m ${rest}s`;
}

/** Row summary with the command's execution time appended once known. */
export function toolSummary(item: ToolItem): string {
  const summary = item.summary ?? "";
  if (
    !summary ||
    item.name !== "run_command" ||
    item.durationMs === undefined
  ) {
    return summary;
  }
  return `${summary} (${formatDuration(item.durationMs)})`;
}

export function categoryLabel(category: ToolCategory, live: boolean): string {
  return live ? CATEGORY_LABELS[category].running : CATEGORY_LABELS[category].done;
}

export function isActive(item: ProcessItem): boolean {
  return (
    (item.kind === "reasoning" && item.streaming) ||
    (item.kind === "tool" &&
      (item.status === "pending" ||
        item.status === "running" ||
        item.status === "awaiting"))
  );
}

/** First non-blank line, used as the settled reasoning preview. */
function firstLine(text: string): string {
  const newline = text.indexOf("\n");
  return (newline === -1 ? text : text.slice(0, newline)).trim();
}

/**
 * Collapsed reasoning preview. While streaming it tracks the latest paragraph
 * whose first line has completed; after settlement it is simply the first line.
 */
export function reasoningSummary(text: string, streaming: boolean): string {
  const clean = text.replace(/\*\*/g, "");
  if (!streaming) return firstLine(clean);
  const paragraphs = clean
    .split(/\r?\n(?:[\t ]*\r?\n)+/)
    .filter((paragraph) => paragraph.trim() !== "");
  for (let index = paragraphs.length - 1; index >= 0; index -= 1) {
    const paragraph = paragraphs[index]!;
    // The final paragraph is still streaming, so it only previews once its
    // first line has completed.
    if (index === paragraphs.length - 1 && !paragraph.includes("\n")) continue;
    return firstLine(paragraph);
  }
  return "";
}

function joinDoneLabels(labels: string[]): string {
  const lower = (label: string) => label.charAt(0).toLowerCase() + label.slice(1);
  if (labels.length === 1) return labels[0] ?? "";
  if (labels.length === 2) return `${labels[0]} and ${lower(labels[1] ?? "")}`;
  return `${labels[0]}, ${lower(labels[1] ?? "")}, and ${lower(labels[2] ?? "")}`;
}

/** Group header: live activity while working, ranked categories once settled. */
export function processTitle(items: readonly ProcessItem[], live: boolean): string {
  if (live) {
    let active: ProcessItem | undefined;
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (isActive(items[index]!)) {
        active = items[index];
        break;
      }
    }
    return active?.kind === "tool"
      ? categoryLabel(toolCategory(active.name), true)
      : "Thinking";
  }

  const counts = new Map<ToolCategory, number>();
  for (const item of items) {
    if (item.kind !== "tool") continue;
    const category = toolCategory(item.name);
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  if (counts.size === 0) return "Analysis completed";
  const ranked = [...counts.entries()]
    .sort((left, right) => right[1] - left[1])
    .slice(0, 3)
    .map(([category]) => categoryLabel(category, false));
  return joinDoneLabels(ranked);
}

/** Category used for the header glyph, matching the current or strongest activity. */
export function primaryCategory(
  items: readonly ProcessItem[],
  live: boolean,
): ToolCategory | "thinking" {
  if (live) {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const item = items[index]!;
      if (item.kind === "tool" && isActive(item)) return toolCategory(item.name);
      if (item.kind === "reasoning" && item.streaming) return "thinking";
    }
    return "thinking";
  }
  const counts = new Map<ToolCategory, number>();
  for (const item of items) {
    if (item.kind !== "tool") continue;
    const category = toolCategory(item.name);
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  let best: ToolCategory | "thinking" = "thinking";
  let bestCount = 0;
  for (const [category, count] of counts) {
    if (count > bestCount) {
      best = category;
      bestCount = count;
    }
  }
  return best;
}
