import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { homeDir } from "@tauri-apps/api/path";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { Chat, type ChatHandle } from "../features/chat/Chat";
import { formatWorkdir } from "../features/chat/chat-model";
import { SessionHistory } from "../features/chat/SessionHistory";
import { WorktreeControl, useSessionWorktrees } from "../features/chat/WorktreeControl";
import type { Attachment } from "../features/chat/attachments";
import type { SendMode } from "../features/chat/session-model";
import { useAgentSession } from "../features/chat/useAgentSession";
import { SettingsPanel } from "../features/settings/SettingsPanel";
import { applyTheme } from "../features/settings/theme";
import { SkillsPopup } from "../features/skills/SkillsPopup";
import { BranchMenu } from "../features/source-control/BranchMenu";
import { SourceControl } from "../features/source-control/SourceControl";
import { useGitBranch } from "../features/source-control/useGitBranch";
import { useGitChanges } from "../features/source-control/useGitChanges";
import { Terminal, type TerminalHandle } from "../features/terminal/Terminal";
import Files, { type FilesHandle } from "../features/files/Files";
import { agentShortcut } from "./shortcuts";
import { Sidebar, type AppView } from "./Sidebar";
import { useWorkspaceConfig } from "./useWorkspaceConfig";
import { useWindowWorkspace } from "./useWindowWorkspace";
import { workspaceFileLink } from "../shared/file-link";
import { errorText } from "../shared/error-text";
import { createWindowCloseHandler } from "./close-window";
import type { AppConfig } from "../features/settings/config";
import "../styles/index.css";

const Architecture = lazy(() => import("../features/architecture/Architecture"));

function App() {
  const [closing, setClosing] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [home, setHome] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [view, setView] = useState<AppView>("chat");
  const [filesOpened, setFilesOpened] = useState(false);
  const [pendingFile, setPendingFile] = useState<{ path: string; workspace: string } | null>(null);
  const [fileWorkspaces, setFileWorkspaces] = useState<string[]>([]);
  const filesRefs = useRef(new Map<string, FilesHandle>());
  const [architectureOpened, setArchitectureOpened] = useState(false);
  const [workspaceTick, setWorkspaceTick] = useState(0);
  // Saving edits changes git status, not the file tree.
  const [savedTick, setSavedTick] = useState(0);
  const chatRef = useRef<ChatHandle>(null);
  const terminalRefs = useRef(new Map<string, TerminalHandle>());
  const [terminalWorkspaces, setTerminalWorkspaces] = useState<string[]>([]);
  const [terminalCommand, setTerminalCommand] = useState<{ workspace: string; text: string } | null>(null);
  const sessionBusyRef = useRef(false);

  const { config, loadFailed, browsingWorkspace, chooseWorkspace, setConfig } =
    useWorkspaceConfig({
      onFirstRun: () => setSettingsOpen(true),
    });
  const { workspace, error: workspaceError } = useWindowWorkspace(config !== null);

  const session = useAgentSession({
    workspace: workspace ?? "",
    enabled: config !== null && workspace !== null,
    onWorkspaceRefresh: () => setWorkspaceTick((value) => value + 1),
  });
  sessionBusyRef.current = session.busy;
  const worktrees = useSessionWorktrees(session.tabs.map((tab) => tab.id), workspaceTick);
  const worktreeInfo = worktrees.infos[session.activeTabId] ?? null;
  const viewWorkspace = worktreeInfo?.directory ?? (session.activeTabId ? "" : (workspace ?? ""));

  async function send(text: string, attachments: Attachment[], mode: SendMode, retryWorktree?: boolean) {
    const id = session.activeTabId;
    try {
      return await session.send(text, attachments, mode, retryWorktree);
    } finally {
      worktrees.setPhase(id, "");
      worktrees.reload();
    }
  }

  async function recoverWorktree(restore: boolean) {
    const id = session.activeTabId;
    try {
      await invoke("session_worktree_recover", { sessionId: id, restore });
      setWorkspaceTick((tick) => tick + 1);
      return true;
    } finally {
      worktrees.setPhase(id, "");
      worktrees.reload();
    }
  }

  useEffect(() => {
    homeDir().then(setHome).catch(() => setHome(""));
  }, []);

  useEffect(() => {
    if (!config || !workspace || home === null) return;
    const title = formatWorkdir(workspace, home);
    document.title = title;
    getCurrentWindow().setTitle(title).catch(() => {});
  }, [config, workspace, home]);

  useEffect(() => {
    if (!config) return;
    applyTheme(config.theme);
    if (config.theme !== "auto") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyTheme("auto");
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [config]);

  useEffect(() => {
    let disposed = false;
    let release: (() => void) | undefined;
    listen<AppConfig>("config://changed", (event) => {
      if (!disposed) setConfig(event.payload);
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      release?.();
    };
  }, [setConfig]);

  useEffect(() => {
    if (config) session.reloadAllowance();
  }, [config?.allowAllCommands]);

  const { changes, changeCount, changesError, changesReady } = useGitChanges({
    enabled: config !== null && Boolean(viewWorkspace) && !worktreeInfo?.missing,
    workspace: viewWorkspace,
    refreshToken: workspaceTick + savedTick,
  });
  const {
    branch,
    branches,
    hasTrackedChanges,
    branchLoaded,
    loading: branchLoading,
    loadError: branchLoadError,
    switchingBranch,
    branchError,
    switchBranch,
    reload: reloadBranches,
    reportBranchError,
    clearBranchError,
  } = useGitBranch({
    active: config !== null && workspace !== null,
    refreshToken: workspaceTick,
    onRefresh: () => setWorkspaceTick((value) => value + 1),
  });

  async function toggleAllowAll() {
    if (!session.allowAll) {
      session.grantAllowAll();
      return;
    }
    const proceed = await confirm(
      "All commands in this workspace will need approval again. Turn off allow all?",
      {
        title: "Allow all commands",
        kind: "warning",
        okLabel: "Require approval",
        cancelLabel: "Keep allowing",
      },
    ).catch(() => false);
    if (proceed) session.revokeAllowAll();
  }

  async function switchWorkspaceBranch(nextBranch: string): Promise<boolean> {
    if (session.running) {
      reportBranchError("Stop the running chat before switching branches.");
      return false;
    }
    if (hasTrackedChanges) {
      const proceed = await confirm(
        "Uncommitted changes in this repository move to the new branch. Switch anyway?",
        {
          title: "Uncommitted changes",
          kind: "warning",
          okLabel: "Switch branch",
          cancelLabel: "Cancel",
        },
      ).catch(() => false);
      if (!proceed) return false;
    }
    const files = workspace ? filesRefs.current.get(workspace) : undefined;
    if (files && !(await files.canLeave())) return false;
    const switched = await switchBranch(nextBranch);
    if (switched) {
      // The open file may have different contents on the new branch.
      files?.closeDocument();
    }
    return switched;
  }

  function newChat() {
    setView("chat");
    session.newTab();
  }

  function selectView(next: AppView) {
    if (next !== view && (next === "git" || next === "chat")) {
      setWorkspaceTick((value) => value + 1);
    }
    if (next === "files") setFilesOpened(true);
    if (next === "architecture") setArchitectureOpened(true);
    setView(next);
  }

  const openFileLink = useCallback((href: string) => {
    const path = workspaceFileLink(href);
    if (!path || !viewWorkspace) return;
    setFilesOpened(true);
    setPendingFile({ path, workspace: viewWorkspace });
    setView("files");
  }, [viewWorkspace]);

  useEffect(() => {
    if (!viewWorkspace || worktreeInfo?.missing) return;
    setTerminalWorkspaces((current) => current.includes(viewWorkspace) ? current : [...current, viewWorkspace]);
    if (filesOpened) {
      setFileWorkspaces((current) => current.includes(viewWorkspace) ? current : [...current, viewWorkspace]);
    }
  }, [viewWorkspace, filesOpened, worktreeInfo?.missing]);

  useEffect(() => {
    if (!pendingFile) return;
    const files = filesRefs.current.get(pendingFile.workspace);
    if (!files) return;
    files.open(pendingFile.path);
    setPendingFile(null);
  }, [fileWorkspaces, pendingFile]);

  useEffect(() => {
    if (!terminalCommand) return;
    const terminal = terminalRefs.current.get(terminalCommand.workspace);
    if (!terminal) return;
    terminal.run(terminalCommand.text);
    setTerminalCommand(null);
  }, [terminalCommand, terminalWorkspaces]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (closing || switchingBranch !== null) {
        event.preventDefault();
        return;
      }
      const action = agentShortcut(event, view, session.running);
      if (!action) return;

      if (settingsOpen) {
        if (action !== "stop") event.preventDefault();
        return;
      }
      if (historyOpen) {
        event.preventDefault();
        if (action === "history") setHistoryOpen(false);
        return;
      }
      if (skillsOpen) {
        event.preventDefault();
        return;
      }

      event.preventDefault();
      if (action === "new-session") {
        newChat();
      } else if (action === "close-session") {
        session.closeTab(session.activeTabId);
      } else if (action === "new-terminal") {
        terminalRefs.current.get(viewWorkspace)?.addTab();
      } else if (action === "quick-open") {
        filesRefs.current.get(viewWorkspace)?.quickOpen();
      } else if (action === "history") {
        // A live run keeps streaming while History is open; picking a session
        // only adds a tab, so the running one is untouched.
        session.refreshHistory();
        setHistoryOpen(true);
      } else {
        session.stop();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let disposed = false;
    const close = createWindowCloseHandler({
      isBusy: () => sessionBusyRef.current,
      confirmStop: () => confirm(
        "This workspace still has a running chat. Close it and stop that work?",
        {
          title: "Close workspace",
          kind: "warning",
          okLabel: "Close and stop",
          cancelLabel: "Keep working",
        },
      ),
      canLeave: async () => {
        for (const files of filesRefs.current.values()) {
          if (!(await files.canLeave())) return false;
        }
        return true;
      },
      prepareClose: () => invoke("close_workspace"),
      destroy: () => appWindow.destroy(),
      onClosing: setClosing,
      onError: async (error) => {
        await message(`Could not close workspace: ${errorText(error)}`, {
          title: "Oaklet", kind: "error",
        }).catch((error) => console.error(error));
      },
    });
    const releases: (() => void)[] = [];
    async function register(listener: Promise<() => void>) {
      const release = await listener;
      if (disposed) release();
      else releases.push(release);
    }
    void Promise.all([
      register(appWindow.onCloseRequested((event) => {
        if (disposed) return;
        event.preventDefault();
        void close();
      })),
      register(appWindow.listen("app://quit_requested", () => {
        if (!disposed) void close();
      })),
    ]).catch((error) => {
      console.error(`Could not install close handlers: ${errorText(error)}`);
    });
    return () => {
      disposed = true;
      for (const release of releases) release();
    };
  }, []);

  if (!config) {
    return (
      <div className="loading">
        {loadFailed ? "Could not load Oaklet." : "Loading Oaklet..."}
      </div>
    );
  }
  if (!workspace) {
    return (
      <div className="loading">
        <p>{workspaceError || "Opening workspace..."}</p>
        {workspaceError && (
          <button type="button" onClick={chooseWorkspace} disabled={browsingWorkspace}>
            Open another workspace
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="app" inert={closing}>
      <Sidebar
        view={view}
        onSelectView={selectView}
        changeCount={changeCount}
        allowAll={session.allowAll}
        onToggleAllowAll={toggleAllowAll}
        onChooseWorkspace={chooseWorkspace}
        browsingWorkspace={browsingWorkspace}
        workspace={workspace}
        branchControl={
          branchLoaded ? (
            <BranchMenu
              iconOnly
              active
              branch={branch}
              branches={branches}
              loading={branchLoading}
              loadError={branchLoadError}
              switchingBranch={switchingBranch}
              branchError={branchError}
              onSwitch={switchWorkspaceBranch}
              onReload={reloadBranches}
              onClearError={clearBranchError}
            />
          ) : null
        }
        skillsOpen={skillsOpen}
        onToggleSkills={() => setSkillsOpen((open) => !open)}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <div
        inert={switchingBranch !== null}
        className={`main${
          view === "chat" && session.items.length === 0 ? " empty-state" : ""
        }`}
      >
        <Chat
          ref={chatRef}
          active={view === "chat"}
          overlayOpen={settingsOpen || historyOpen || skillsOpen}
          tabs={session.tabs}
          activeTabId={session.activeTabId}
          onSelectTab={session.selectTab}
          onNewTab={session.newTab}
          onMoveTab={session.moveTab}
          onCloseTab={session.closeTab}
          items={session.items}
          pending={session.pending}
          firstSeq={session.firstSeq}
          hasMore={session.hasMore}
          loadingOlder={session.loadingOlder}
          loadOlderError={session.loadOlderError}
          running={session.running}
          compacting={session.compacting}
          activeRunId={session.activeRunId}
          context={session.context}
          usage={session.usage}
          stepUsage={session.stepUsage}
          model={config.model}
          onSend={send}
          onStop={session.stop}
          onCompact={session.compact}
          onLoadOlder={session.loadOlder}
          onApproval={session.respondToApproval}
          onConfirmPlan={session.confirmPlan}
          onCheckPlanBaseline={session.checkPlanBaseline}
          onLoadPlanCompare={session.loadPlanCompare}
          onOpenFile={openFileLink}
          onSendToTerminal={(text) => {
            if (!viewWorkspace || worktreeInfo?.missing) return;
            setView("terminal");
            setTerminalCommand({ workspace: viewWorkspace, text });
          }}
          worktrees={worktrees.infos}
          checkoutControl={worktreeInfo ? (retry) => (
            <WorktreeControl
              key={session.activeTabId}
              info={worktreeInfo}
              phase={worktrees.phases[session.activeTabId] ?? ""}
              retry={(keepRequest) => retry(keepRequest, worktreeInfo.waitingText ?? "")}
              stop={() => { void invoke("session_stop", { sessionId: session.activeTabId }); }}
              recover={recoverWorktree}
            />
          ) : null}
        />

        {terminalWorkspaces.map((directory) => (
          <Terminal
            key={directory}
            ref={(handle) => {
              if (handle) terminalRefs.current.set(directory, handle);
              else terminalRefs.current.delete(directory);
            }}
            workspace={directory}
            active={view === "terminal" && directory === viewWorkspace && !worktreeInfo?.missing}
            onClose={() => selectView("chat")}
          />
        ))}

        {viewWorkspace && (
          <SourceControl
            key={viewWorkspace}
            active={view === "git"}
            workspace={viewWorkspace}
            changes={changes}
            changesError={changesError}
            changesReady={changesReady}
            refreshToken={workspaceTick + savedTick}
            onRefresh={() => setWorkspaceTick((value) => value + 1)}
          />
        )}

        {fileWorkspaces.map((directory) => (
          <Files
            key={directory}
            ref={(handle) => {
              if (handle) filesRefs.current.set(directory, handle);
              else filesRefs.current.delete(directory);
            }}
            active={view === "files" && directory === viewWorkspace && !worktreeInfo?.missing}
            workspace={directory}
            refreshToken={workspaceTick}
            changes={directory === viewWorkspace ? changes : null}
            onRefresh={() => setWorkspaceTick((value) => value + 1)}
            onSaved={() => setSavedTick((value) => value + 1)}
          />
        ))}

        {architectureOpened && viewWorkspace && (
          <Suspense fallback={<div className="architecture-loading">Loading architecture…</div>}>
            <Architecture
              key={viewWorkspace}
              active={view === "architecture"}
              workspace={viewWorkspace}
              refreshToken={workspaceTick}
              running={session.running}
              onPrompt={(prompt) => {
                setView("chat");
                chatRef.current?.prefill(prompt);
              }}
              onRefresh={() => setWorkspaceTick((value) => value + 1)}
            />
          </Suspense>
        )}
      </div>

      {settingsOpen && (
        <SettingsPanel
          config={config}
          firstRun={!config.apiKey}
          onClose={() => setSettingsOpen(false)}
          onSaved={(saved) => {
            setConfig(saved);
            setSettingsOpen(false);
            session.reloadAllowance();
          }}
        />
      )}

      {historyOpen && (
        <SessionHistory
          sessions={session.history}
          onArchive={async (id, archived) => {
            const status = await invoke<string | null>("session_archive", { sessionId: id, archived });
            if (archived && session.tabs.some((tab) => tab.id === id)) void session.closeTab(id);
            session.refreshHistory();
            return status;
          }}
          onClose={() => setHistoryOpen(false)}
          onPick={(id) => {
            session.restore(id);
            setView("chat");
            setHistoryOpen(false);
          }}
        />
      )}

      {skillsOpen && viewWorkspace && (
        <SkillsPopup
          key={viewWorkspace}
          workspace={viewWorkspace}
          onClose={() => setSkillsOpen(false)}
        />
      )}
    </div>
  );
}

export default App;
