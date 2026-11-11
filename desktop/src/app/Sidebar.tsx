import {
  FolderOpenRegular,
  FolderTreeRegular,
  MindMapRegular,
  GitBranch2Regular,
  Message4Regular,
  Settings5Regular,
  Sparkles3Regular,
  TerminalBoxRegular,
} from "@mingcute/react/core-regular";
import { LeafFilled } from "@mingcute/react/core-filled";

export type AppView = "chat" | "terminal" | "git" | "architecture" | "files";

export function Sidebar({
  view,
  onSelectView,
  unstaged,
  allowAll,
  onRevokeAllowAll,
  onChooseWorkspace,
  browsingWorkspace,
  workspace,
  skillsOpen,
  onToggleSkills,
  onOpenSettings,
}: {
  view: AppView;
  onSelectView: (view: AppView) => void;
  unstaged: number | null;
  allowAll: boolean;
  onRevokeAllowAll: () => void;
  onChooseWorkspace: () => void;
  browsingWorkspace: boolean;
  workspace: string;
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
        className={`ghost icon-button sc-toggle${
          view === "git" ? " active" : ""
        }`}
        onClick={() => onSelectView("git")}
        aria-label={
          unstaged
            ? `Source control (${unstaged} unstaged)`
            : "Source control"
        }
        aria-pressed={view === "git"}
        title={
          unstaged
            ? `${unstaged} file${unstaged === 1 ? "" : "s"} with changes not staged for commit`
            : "Source control"
        }
      >
        <GitBranch2Regular size={18} />
        {unstaged !== null && unstaged > 0 && (
          <span className="sc-badge" aria-hidden="true">
            {unstaged > 99 ? "99+" : unstaged}
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
        <MindMapRegular size={18} />
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
      <div className="sidebar-foot">
        {allowAll && (
          <button
            type="button"
            className="ghost icon-button allow-all-chip"
            onClick={onRevokeAllowAll}
            aria-label="Revoke allowance for all commands"
            title="All commands in this workspace run without approval. Click to ask again."
          >
            <LeafFilled size={16} />
          </button>
        )}
        <button
          type="button"
          className="ghost icon-button"
          onClick={onChooseWorkspace}
          disabled={browsingWorkspace}
          aria-label="Open workspace folder"
          title={`Workspace folder: ${workspace}`}
        >
          <FolderOpenRegular size={16} />
        </button>
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
