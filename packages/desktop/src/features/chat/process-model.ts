export type ToolStatus = "pending" | "awaiting" | "running" | "ok" | "error";

export type ApprovalChoice = "deny" | "allow" | "allowAll";

/** One changed file fragment supplied by the backend for a mutation tool. */
export type FileDiff = {
  path: string;
  oldText: string | null;
  newText: string;
};

/** Real process outcome carried by a command tool result. */
export type ProcessOutcome =
  | { kind: "succeeded" }
  | { kind: "exited"; code: number }
  | { kind: "signaled"; signal: number; signalName: string }
  | { kind: "timed_out" }
  | { kind: "cancelled" }
  | { kind: "denied" }
  | { kind: "running"; jobId: string };

export type ReasoningItem = {
  kind: "reasoning";
  seq?: number;
  runId: string;
  text: string;
  streaming: boolean;
  startedAt?: number;
  durationMs?: number;
};

export type ToolItem = {
  kind: "tool";
  seq?: number;
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
  diffs?: FileDiff[];
  /** Absent on conversations recorded before command outcomes were saved. */
  outcome?: ProcessOutcome;
};

export type ProcessItem = ReasoningItem | ToolItem;

export type ToolCategory = "read" | "write" | "edit" | "command" | "web" | "tools";

const TOOL_CATEGORIES: Record<string, ToolCategory> = {
  read_file: "read",
  read_image: "read",
  list_dir: "read",
  glob: "read",
  grep: "read",
  write_file: "write",
  edit_file: "edit",
  apply_patch: "edit",
  run_command: "command",
  job_list: "read",
  job_output: "read",
  job_kill: "command",
  web_search: "web",
  web_fetch: "web",
};

const CATEGORY_LABELS: Record<ToolCategory, { running: string; done: string }> = {
  read: { running: "Reading files", done: "Read files" },
  write: { running: "Writing files", done: "Wrote files" },
  edit: { running: "Editing files", done: "Edited files" },
  command: { running: "Running commands", done: "Ran commands" },
  web: { running: "Searching the web", done: "Searched the web" },
  tools: { running: "Calling tools", done: "Called tools" },
};

const TOOL_TITLES: Record<string, string> = {
  read_file: "Read file",
  read_image: "Read image",
  list_dir: "List folder",
  glob: "Find files",
  grep: "Search contents",
  write_file: "Write file",
  edit_file: "Edit file",
  apply_patch: "Apply patch",
  run_command: "Run command",
  job_list: "List jobs",
  job_output: "Job output",
  job_kill: "Stop job",
  web_fetch: "Fetch page",
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

/** Short row label for a command outcome; older rows have none. */
export function outcomeLabel(outcome?: ProcessOutcome): string | undefined {
  if (!outcome) return undefined;
  switch (outcome.kind) {
    case "succeeded":
      return undefined;
    case "exited":
      return outcome.code === 0 ? undefined : `Exit ${outcome.code}`;
    case "signaled":
      return outcome.signalName || `Signal ${outcome.signal}`;
    case "timed_out":
      return "Timed out";
    case "cancelled":
      return "Stopped";
    case "denied":
      return "Denied";
    case "running":
      return "In background";
  }
}

/** Expanded detail that explains the exit or termination. */
export function outcomeDetail(outcome?: ProcessOutcome): string | undefined {
  if (!outcome) return undefined;
  switch (outcome.kind) {
    case "succeeded":
      return undefined;
    case "exited":
      return outcome.code === 0 ? undefined : `Exit code ${outcome.code}.`;
    case "signaled":
      return `Terminated by ${outcome.signalName} (signal ${outcome.signal}).`;
    case "timed_out":
      return "The command timed out.";
    case "cancelled":
      return "The command was cancelled.";
    case "denied":
      return "The command was denied.";
    case "running":
      return `Background job ${outcome.jobId} is running. Read its result with job_output.`;
  }
}

const OPENED_FILE_TOOLS = new Set(["read_file"]);

/** Output worth showing; a bare cancellation notice only repeats the detail. */
export function toolOutput(item: ToolItem): string | undefined {
  if (!item.output) return undefined;
  if (item.outcome?.kind === "cancelled" && item.output.trim() === "Cancelled.") {
    return undefined;
  }
  if (item.status !== "error" && OPENED_FILE_TOOLS.has(item.name)) return undefined;
  return item.output;
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

/** Elapsed time, kept separate from the truncated summary. */
export function processDuration(item: ProcessItem, now: number): string | undefined {
  if (
    item.kind === "tool" &&
    (item.name !== "run_command" || item.status === "awaiting")
  ) return undefined;
  if (item.durationMs !== undefined) return formatDuration(item.durationMs);
  if (isActive(item) && item.startedAt !== undefined) {
    return formatDuration(now - item.startedAt);
  }
  return undefined;
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
