import { useEffect, useState, type ReactNode } from "react";
import {
  BookOpen,
  CaretDown,
  CircleNotch,
  Hourglass,
  LightbulbFilament,
  PencilLine,
  PencilSimple,
  Sparkle,
  Terminal,
  XCircle,
} from "@phosphor-icons/react";
import { MarkdownText } from "./Markdown";
import {
  isActive,
  primaryCategory,
  processTitle,
  reasoningSummary,
  toolCategory,
  toolTitle,
  type ApprovalChoice,
  type ProcessItem,
  type ReasoningItem,
  type ToolCategory,
  type ToolItem,
} from "./process-model";

const CATEGORY_ICONS: Record<ToolCategory, ReactNode> = {
  read: <BookOpen size={14} />,
  write: <PencilSimple size={14} />,
  edit: <PencilLine size={14} />,
  command: <Terminal size={14} />,
  tools: <Sparkle size={14} />,
};

function categoryIcon(category: ToolCategory | "thinking"): ReactNode {
  return category === "thinking" ? (
    <LightbulbFilament size={14} />
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
  onToggle,
}: {
  icon: ReactNode;
  title: string;
  summary?: string;
  live?: boolean;
  open: boolean;
  expandable: boolean;
  trailing?: ReactNode;
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
      {trailing}
      {expandable && <CaretDown size={12} className="row-chevron" weight="bold" />}
    </button>
  );
}

function ReasoningRow({ item }: { item: ReasoningItem }) {
  const [open, setOpen] = useState(false);
  const summary = reasoningSummary(item.text, item.streaming);
  return (
    <div className="process-row reasoning">
      <DisclosureHead
        icon={<LightbulbFilament size={14} />}
        title="Thinking"
        summary={summary}
        live={item.streaming}
        open={open}
        expandable={item.text.trim() !== ""}
        trailing={item.streaming ? <CircleNotch size={12} className="spin" /> : undefined}
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
      <p>{item.approvalPrompt ?? "Approve this tool call?"}</p>
      <pre>{item.approvalDetail ?? ""}</pre>
      <div className="approval-actions">
        <button type="button" onClick={() => onApproval(item, "deny")}>
          Deny
        </button>
        {item.name === "run_command" && (
          <button
            type="button"
            onClick={() => onApproval(item, "allowAll")}
            title="Run every command in this workspace without asking until you turn it off."
          >
            Allow all in workspace
          </button>
        )}
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
  const hasOutput = Boolean(item.output);
  const statusLabel = running
    ? "Running"
    : item.status === "awaiting"
      ? "Awaiting approval"
      : item.status === "error"
        ? "Failed"
        : "";

  useEffect(() => {
    if (item.status === "error") setOpen(true);
  }, [item.status]);

  const trailing = running ? (
    <CircleNotch size={12} className="spin" />
  ) : item.status === "awaiting" ? (
    <Hourglass size={12} className="awaiting-icon" />
  ) : item.status === "error" ? (
    <XCircle size={13} className="error-icon" weight="fill" />
  ) : undefined;

  return (
    <div className="process-row tool" data-status={item.status}>
      {statusLabel && <span className="sr-only">{statusLabel}</span>}
      <DisclosureHead
        icon={categoryIcon(category)}
        title={toolTitle(item.name)}
        summary={item.summary}
        live={running}
        open={open}
        expandable={hasOutput}
        trailing={trailing}
        onToggle={() => setOpen((value) => !value)}
      />
      {item.status === "awaiting" && (
        <Approval item={item} onApproval={onApproval} />
      )}
      {open && item.output && (
        <div className="tool-body">
          <pre>{item.output}</pre>
        </div>
      )}
    </div>
  );
}

export function ProcessGroup({
  items,
  live,
  onApproval,
}: {
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
    <div className="process">
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
        <CaretDown size={13} className="process-chevron" weight="bold" />
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
