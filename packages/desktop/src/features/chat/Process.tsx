import { useEffect, useMemo, useState, type ReactNode } from "react";
import {
  BookOpenTextRegular,
  BulbRegular,
  DownSmallRegular,
  LoadingRegular,
  Pencil2Regular,
  PencilRegular,
  SandglassRegular,
  SearchAIRegular,
  Sparkles3Regular,
  Task2Regular,
  TerminalBoxRegular,
} from "@mingcute/react/core-regular";
import { WarningFilled } from "@mingcute/react/core-filled";
import { MarkdownText } from "../../shared/Markdown";
import { DiffCard } from "./DiffCard";
import { diffTotals } from "./tool-diff";
import {
  isActive,
  outcomeDetail,
  outcomeLabel,
  primaryCategory,
  processTitle,
  processDuration,
  reasoningSummary,
  toolCategory,
  toolOutput,
  toolTitle,
  type ApprovalChoice,
  type ProcessItem,
  type ReasoningItem,
  type ToolCategory,
  type ToolItem,
} from "./process-model";

const CATEGORY_ICONS: Record<ToolCategory, ReactNode> = {
  read: <BookOpenTextRegular size={14} />,
  write: <PencilRegular size={14} />,
  edit: <Pencil2Regular size={14} />,
  command: <TerminalBoxRegular size={14} />,
  web: <SearchAIRegular size={14} />,
  tools: <Sparkles3Regular size={14} />,
};

/** Tools that read better with their own glyph than their category's. */
const TOOL_ICONS: Record<string, ReactNode> = {
  job_output: <Task2Regular size={14} />,
};

function categoryIcon(category: ToolCategory | "thinking"): ReactNode {
  return category === "thinking" ? (
    <BulbRegular size={14} />
  ) : (
    CATEGORY_ICONS[category]
  );
}

function DisclosureHead({
  icon,
  title,
  summary,
  live,
  open,
  expandable,
  trailing,
  totals,
  onToggle,
}: {
  icon: ReactNode;
  title: string;
  summary?: string;
  live?: boolean;
  open: boolean;
  expandable: boolean;
  trailing?: ReactNode;
  totals?: { added: number; removed: number };
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      className="row-head"
      aria-expanded={expandable ? open : undefined}
      disabled={!expandable}
      onClick={onToggle}
    >
      <span className="row-icon">{icon}</span>
      <span className="row-title">{title}</span>
      {summary && (
        <>
          <span className="row-dot" aria-hidden />
          <span className={`row-summary${live ? " shimmer" : ""}`} title={summary}>
            {summary}
          </span>
        </>
      )}
      {totals !== undefined && (totals.added > 0 || totals.removed > 0) && (
        <span className="row-diff-stat">
          <span className="add">+{totals.added}</span>
          <span className="del">-{totals.removed}</span>
        </span>
      )}
      {trailing}
      {expandable && <DownSmallRegular size={12} className="row-chevron" />}
    </button>
  );
}

function ExecutionTime({ item }: { item: ProcessItem }) {
  const [now, setNow] = useState(Date.now);
  const live = isActive(item) && (item.kind === "reasoning" || item.status !== "awaiting");
  const duration = processDuration(item, now);

  useEffect(() => {
    if (!live || item.startedAt === undefined) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [item.startedAt, live]);

  return duration === undefined ? null : (
    <span className="row-duration" title={`Execution time: ${duration}`}>
      {duration}
    </span>
  );
}

function ReasoningRow({ item }: { item: ReasoningItem }) {
  const [open, setOpen] = useState(false);
  const summary = reasoningSummary(item.text, item.streaming);
  return (
    <div className="process-row reasoning">
      <DisclosureHead
        icon={<BulbRegular size={14} />}
        title="Thinking"
        summary={summary}
        live={item.streaming}
        open={open}
        expandable={item.text.trim() !== ""}
        trailing={
          <>
            <ExecutionTime item={item} />
            {item.streaming && <LoadingRegular size={12} className="spin" />}
          </>
        }
        onToggle={() => setOpen((value) => !value)}
      />
      {open && (
        <div className="reasoning-body">
          <MarkdownText text={item.text} compact />
        </div>
      )}
    </div>
  );
}

function Approval({
  item,
  onApproval,
}: {
  item: ToolItem;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
}) {
  return (
    <div className="approval">
      <p className="approval-prompt">
        <WarningFilled size={14} aria-hidden />
        {item.approvalPrompt ?? "Approve this tool call?"}
      </p>
      <pre>{item.approvalDetail ?? ""}</pre>
      <div className="approval-actions">
        {item.name === "run_command" && (
          <button
            type="button"
            className="ghost approval-allow-all"
            onClick={() => onApproval(item, "allowAll")}
            title="Run every command in this workspace without asking until you turn it off."
          >
            Allow all in workspace
          </button>
        )}
        <button type="button" onClick={() => onApproval(item, "deny")}>
          Deny
        </button>
        <button
          type="button"
          className="primary"
          onClick={() => onApproval(item, "allow")}
        >
          Allow
        </button>
      </div>
    </div>
  );
}

function ToolRow({
  item,
  onApproval,
}: {
  item: ToolItem;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
}) {
  const [open, setOpen] = useState(false);
  const category = toolCategory(item.name);
  const running = item.status === "pending" || item.status === "running";
  const output = toolOutput(item);
  const diffs = item.diffs ?? [];
  const hasDiff = diffs.length > 0;
  const totals = useMemo(
    () => (hasDiff ? diffTotals(diffs) : undefined),
    [diffs, hasDiff],
  );
  const statusLabel = running
    ? "Running"
    : item.status === "awaiting"
      ? "Awaiting approval"
      : item.status === "error"
        ? "Failed"
        : "";
  const outcome = outcomeLabel(item.outcome);
  const detail = outcomeDetail(item.outcome);

  useEffect(() => {
    if (item.status === "error") setOpen(true);
  }, [item.status]);

  const trailing = running ? (
    <LoadingRegular size={12} className="spin" />
  ) : item.status === "awaiting" ? (
    <SandglassRegular size={12} className="awaiting-icon" />
  ) : undefined;

  return (
    <div className="process-row tool" data-status={item.status}>
      {statusLabel && <span className="sr-only">{statusLabel}</span>}
      <DisclosureHead
        icon={TOOL_ICONS[item.name] ?? categoryIcon(category)}
        title={toolTitle(item.name)}
        summary={item.summary}
        live={running}
        open={open}
        expandable={Boolean(output) || hasDiff || Boolean(detail)}
        trailing={
          <>
            {outcome && (
              <span
                className="row-outcome"
                data-tone={item.status === "error" ? "error" : "muted"}
              >
                {outcome}
              </span>
            )}
            {item.name === "run_command" && <ExecutionTime item={item} />}
            {trailing}
          </>
        }
        totals={totals}
        onToggle={() => setOpen((value) => !value)}
      />
      {item.status === "awaiting" && (
        <Approval item={item} onApproval={onApproval} />
      )}
      {open && hasDiff && <DiffCard diffs={diffs} />}
      {open && !hasDiff && (output || detail) && (
        <div className="tool-body">
          {detail && <p className="tool-outcome-detail">{detail}</p>}
          {output && <pre>{output}</pre>}
        </div>
      )}
    </div>
  );
}

export function ProcessGroup({
  seq,
  items,
  live,
  onApproval,
}: {
  seq?: number;
  items: readonly ProcessItem[];
  live: boolean;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
}) {
  // An approval must stay visible: collapsing the group must not hide the
  // only way to unblock the run.
  const blocked = items.some(
    (item) => item.kind === "tool" && item.status === "awaiting",
  );
  const failed = items.some(
    (item) => item.kind === "tool" && item.status === "error",
  );
  const [open, setOpen] = useState(live || blocked || failed);
  const working = live && items.some(isActive);
  const expanded = open || blocked;

  useEffect(() => {
    if (!live && !failed) setOpen(false);
  }, [failed, live]);

  useEffect(() => {
    if (blocked) setOpen(true);
  }, [blocked]);

  return (
    <div className="process" data-seq={seq}>
      <button
        type="button"
        className="process-head"
        aria-expanded={expanded}
        onClick={() => {
          if (!blocked) setOpen((value) => !value);
        }}
      >
        <span className="process-icon">
          {categoryIcon(primaryCategory(items, working))}
        </span>
        <span className={`process-title${working ? " shimmer" : ""}`}>
          {processTitle(items, working)}
        </span>
        <DownSmallRegular size={13} className="process-chevron" />
      </button>
      {expanded && (
        <div className="process-body">
          {items.map((item, index) =>
            item.kind === "reasoning" ? (
              <ReasoningRow key={`reasoning-${index}`} item={item} />
            ) : (
              <ToolRow
                key={`tool-${item.id}`}
                item={item}
                onApproval={onApproval}
              />
            ),
          )}
        </div>
      )}
    </div>
  );
}
