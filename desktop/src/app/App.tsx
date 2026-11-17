import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import { Chat, type ChatHandle } from "../features/chat/Chat";
import { formatWorkdir } from "../features/chat/chat-model";
import { SessionHistory } from "../features/chat/SessionHistory";
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
import type { AppConfig } from "../features/settings/config";
import "../styles/index.css";

const Architecture = lazy(() => import("../features/architecture/Architecture"));

function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [home, setHome] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [view, setView] = useState<AppView>("chat");
  const [filesOpened, setFilesOpened] = useState(false);
  const [pendingFile, setPendingFile] = useState<string | null>(null);
  const filesRef = useRef<FilesHandle>(null);
  const [architectureOpened, setArchitectureOpened] = useState(false);
  const [workspaceTick, setWorkspaceTick] = useState(0);
  // Saving edits changes git status, not the file tree.
  const [savedTick, setSavedTick] = useState(0);
  const chatRef = useRef<ChatHandle>(null);
  const terminalRef = useRef<TerminalHandle>(null);
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

  const { changes, changeCount, changesError } = useGitChanges({
    enabled: config !== null && workspace !== null,
    workspace: workspace ?? "",
    refreshToken: workspaceTick + savedTick,
  });
  const {
    branch,
    branches,
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

  async function switchWorkspaceBranch(nextBranch: string): Promise<boolean> {
    if (session.running) {
      reportBranchError("Stop the running chat before switching branches.");
      return false;
    }
    const tracked =
      changes?.some((change) => change.status !== "untracked") ?? false;
    if (tracked) {
      const proceed = await confirm(
        "Uncommitted changes stay in this workspace and move to the new branch. Switch anyway?",
        {
          title: "Uncommitted changes",
          kind: "warning",
          okLabel: "Switch branch",
          cancelLabel: "Cancel",
        },
      ).catch(() => false);
      if (!proceed) return false;
    }
    if (filesRef.current) {
      if (!(await filesRef.current.canLeave())) return false;
      // The open file may not exist, or may have different contents, on the
      // branch being checked out.
      filesRef.current.closeDocument();
    }

    return switchBranch(nextBranch);
  }

  function newChat() {
    setView("chat");
    session.newTab();
  }

  function selectView(next: AppView) {
    if (next === "git" && view !== "git") {
      setWorkspaceTick((value) => value + 1);
    }
    if (next === "files") setFilesOpened(true);
    if (next === "architecture") setArchitectureOpened(true);
    setView(next);
  }

  const openFileLink = useCallback((href: string) => {
    const path = workspaceFileLink(href);
    if (!path) return;
    setFilesOpened(true);
    setPendingFile(path);
    setView("files");
  }, []);

  // Files mounts lazily; hold the target until its handle exists.
  useEffect(() => {
    if (pendingFile === null || !filesOpened) return;
    filesRef.current?.open(pendingFile);
    setPendingFile(null);
  }, [filesOpened, pendingFile]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
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
        terminalRef.current?.addTab();
      } else if (action === "history") {
        if (!session.running) {
          session.refreshHistory();
          setHistoryOpen(true);
        }
      } else {
        session.stop();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  useEffect(() => {
    if (!workspace) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    const unlisten = appWindow.onCloseRequested(async (event) => {
      try {
        if (disposed) return;
        const busy = sessionBusyRef.current;
        const dirty = filesRef.current?.hasUnsaved() ?? false;
        if (!busy && !dirty) return;

        event.preventDefault();
        if (busy) {
          const close = await confirm(
            "This workspace still has a running chat. Close it and stop that work?",
            {
              title: "Close workspace",
              kind: "warning",
              okLabel: "Close and stop",
              cancelLabel: "Keep working",
            },
          );
          if (!close) return;
        }
        if (dirty && !(await filesRef.current?.canLeave())) return;
        await appWindow.destroy();
      } catch {
        // Keep the window open if a confirmation could not be shown.
      }
    });
    unlisten.catch(() => {});
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop()).catch(() => {});
    };
  }, [workspace]);

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
    <div className="app">
      <Sidebar
        view={view}
        onSelectView={selectView}
        changeCount={changeCount}
        allowAll={session.allowAll}
        onRevokeAllowAll={session.revokeAllowAll}
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
        className={`main${
          view === "chat" && session.items.length === 0 ? " empty-state" : ""
        }`}
      >
        <Chat
          ref={chatRef}
          active={view === "chat"}
          tabs={session.tabs}
          activeTabId={session.activeTabId}
          onSelectTab={session.selectTab}
          onNewTab={session.newTab}
          onCloseTab={session.closeTab}
          items={session.items}
          firstSeq={session.firstSeq}
          hasMore={session.hasMore}
          loadingOlder={session.loadingOlder}
          loadOlderError={session.loadOlderError}
          running={session.running}
          compacting={session.compacting}
          activeRunId={session.activeRunId}
          context={session.context}
          model={config.model}
          onSend={session.send}
          onStop={session.stop}
          onCompact={session.compact}
          onLoadOlder={session.loadOlder}
          onApproval={session.respondToApproval}
          onOpenFile={openFileLink}
          onSendToTerminal={(text) => {
            setView("terminal");
            terminalRef.current?.run(text);
          }}
        />

        <Terminal
          ref={terminalRef}
          key={workspace}
          active={view === "terminal"}
          onClose={() => setView("chat")}
        />

        <SourceControl
          key={workspace}
          active={view === "git"}
          changes={changes}
          changesError={changesError}
          refreshToken={workspaceTick}
          onRefresh={() => setWorkspaceTick((value) => value + 1)}
        />

        {filesOpened && (
          <Files
            ref={filesRef}
            key={workspace}
            active={view === "files"}
            workspace={workspace}
            refreshToken={workspaceTick}
            changes={changes}
            onRefresh={() => setWorkspaceTick((value) => value + 1)}
            onSaved={() => setSavedTick((value) => value + 1)}
          />
        )}

        {architectureOpened && (
          <Suspense
            fallback={
              <div className="arch-loading">Loading architecture...</div>
            }
          >
            <Architecture
              key={workspace}
              active={view === "architecture"}
              workspace={workspace}
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
          onClose={() => setHistoryOpen(false)}
          onPick={(id) => {
            session.restore(id);
            setView("chat");
            setHistoryOpen(false);
          }}
        />
      )}

      {skillsOpen && (
        <SkillsPopup
          key={workspace}
          onClose={() => setSkillsOpen(false)}
        />
      )}
    </div>
  );
}

export default App;
