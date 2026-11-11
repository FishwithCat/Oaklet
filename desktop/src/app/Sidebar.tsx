import {
  ChatDotsIcon,
  FolderOpenIcon,
  FilesIcon,
  GearIcon,
  GitBranchIcon,
  LeafIcon,
  SparkleIcon,
  TerminalWindowIcon,
  TreeStructureIcon,
} from "@phosphor-icons/react";

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
        <ChatDotsIcon size={18} />
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
        <TerminalWindowIcon size={18} />
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
        <GitBranchIcon size={18} />
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
        <TreeStructureIcon size={18} />
      </button>
      <button
        type="button"
        className={`ghost icon-button files-toggle${view === "files" ? " active" : ""}`}
        onClick={() => onSelectView("files")}
        aria-label="Files"
        aria-pressed={view === "files"}
        title="Files"
      >
        <FilesIcon size={18} />
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
            <LeafIcon size={16} weight="fill" />
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
          <FolderOpenIcon size={16} />
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
          <SparkleIcon size={16} />
        </button>
        <button
          type="button"
          className="ghost icon-button"
          onClick={onOpenSettings}
          aria-label="Settings"
          title="Settings"
        >
          <GearIcon size={16} />
        </button>
      </div>
    </aside>
  );
}
