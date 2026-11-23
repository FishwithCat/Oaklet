import type { ReactNode } from "react";
import {
  FolderOpenRegular,
  FolderTreeRegular,
  LighthouseRegular,
  GitCommitHorizontalRegular,
  Message4Regular,
  SafeLockRegular,
  Settings5Regular,
  Sparkles3Regular,
  TerminalBoxRegular,
} from "@mingcute/react/core-regular";
import { FlashFilled } from "@mingcute/react/core-filled";

export type AppView = "chat" | "terminal" | "git" | "architecture" | "files";

export function Sidebar({
  view,
  onSelectView,
  changeCount,
  allowAll,
  onToggleAllowAll,
  onChooseWorkspace,
  browsingWorkspace,
  workspace,
  branchControl,
  skillsOpen,
  onToggleSkills,
  onOpenSettings,
}: {
  view: AppView;
  onSelectView: (view: AppView) => void;
  changeCount: number | null;
  allowAll: boolean;
  onToggleAllowAll: () => void;
  onChooseWorkspace: () => void;
  browsingWorkspace: boolean;
  workspace: string;
  branchControl?: ReactNode;
  skillsOpen: boolean;
  onToggleSkills: () => void;
  onOpenSettings: () => void;
}) {
  return (
    <aside className="sidebar">
      <button
        type="button"
        className={`ghost icon-button new-chat${
          view === "chat" ? " active" : ""
        }`}
        onClick={() => onSelectView("chat")}
        aria-label="Chat"
        title="Chat"
      >
        <Message4Regular size={18} />
      </button>
      <button
        type="button"
        className={`ghost icon-button terminal-toggle${
          view === "terminal" ? " active" : ""
        }`}
        onClick={() => onSelectView("terminal")}
        aria-label="Terminal"
        aria-pressed={view === "terminal"}
        title="Terminal"
      >
        <TerminalBoxRegular size={18} />
      </button>
      <button
        type="button"
        className={`ghost icon-button files-toggle${view === "files" ? " active" : ""}`}
        onClick={() => onSelectView("files")}
        aria-label="Files"
        aria-pressed={view === "files"}
        title="Files"
      >
        <FolderTreeRegular size={18} />
      </button>
      <button
        type="button"
        className={`ghost icon-button sc-toggle${
          view === "git" ? " active" : ""
        }`}
        onClick={() => onSelectView("git")}
        aria-label={
          changeCount
            ? `Source control (${changeCount} uncommitted)`
            : "Source control"
        }
        aria-pressed={view === "git"}
        title={
          changeCount
            ? `${changeCount} file${changeCount === 1 ? "" : "s"} with uncommitted changes`
            : "Source control"
        }
      >
        <GitCommitHorizontalRegular size={18} />
        {changeCount !== null && changeCount > 0 && (
          <span className="sc-badge" aria-hidden="true">
            {changeCount > 99 ? "99+" : changeCount}
          </span>
        )}
      </button>
      <button
        type="button"
        className={`ghost icon-button arch-toggle${
          view === "architecture" ? " active" : ""
        }`}
        onClick={() => onSelectView("architecture")}
        aria-label="Architecture"
        aria-pressed={view === "architecture"}
        title="Architecture"
      >
        <LighthouseRegular size={18} />
      </button>
      <div className="sidebar-foot">
        <button
          type="button"
          className={`ghost icon-button allow-all-chip${
            allowAll ? " allow-all-on" : ""
          }`}
          onClick={onToggleAllowAll}
          aria-label={
            allowAll
              ? "Require approval for all commands"
              : "Allow all commands without approval"
          }
          aria-pressed={allowAll}
          title={
            allowAll
              ? "All commands in this workspace run without approval. Click to ask again."
              : "Commands in this workspace ask for approval. Click to allow all without asking."
          }
        >
          {allowAll ? <FlashFilled size={16} /> : <SafeLockRegular size={16} />}
        </button>
        <button
          type="button"
          className="ghost icon-button"
          onClick={onChooseWorkspace}
          disabled={browsingWorkspace}
          aria-label="Open workspace"
          title={`Open another workspace (current: ${workspace})`}
        >
          <FolderOpenRegular size={16} />
        </button>
        {branchControl && <div className="sidebar-branch">{branchControl}</div>}
        <button
          type="button"
          className={`ghost icon-button skills-toggle${
            skillsOpen ? " active" : ""
          }`}
          onClick={onToggleSkills}
          aria-label="Skills"
          aria-pressed={skillsOpen}
          title="Skills"
        >
          <Sparkles3Regular size={16} />
        </button>
        <button
          type="button"
          className="ghost icon-button"
          onClick={onOpenSettings}
          aria-label="Settings"
          title="Settings"
        >
          <Settings5Regular size={16} />
        </button>
      </div>
    </aside>
  );
}
