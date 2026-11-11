import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { homeDir } from "@tauri-apps/api/path";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Chat, type ChatHandle } from "../features/chat/Chat";
import { formatWorkdir } from "../features/chat/chat-model";
import { SessionHistory } from "../features/chat/SessionHistory";
import { useAgentSession } from "../features/chat/useAgentSession";
import { SettingsPanel } from "../features/settings/SettingsPanel";
import { SkillsPopup } from "../features/skills/SkillsPopup";
import { SourceControl } from "../features/source-control/SourceControl";
import { useGitChanges } from "../features/source-control/useGitChanges";
import { Terminal, type TerminalHandle } from "../features/terminal/Terminal";
import type { FilesHandle } from "../features/files/Files";
import { agentShortcut } from "./shortcuts";
import { Sidebar, type AppView } from "./Sidebar";
import { useWorkspaceConfig } from "./useWorkspaceConfig";
import "../styles/index.css";

const Files = lazy(() => import("../features/files/Files"));
const Architecture = lazy(() => import("../features/architecture/Architecture"));

function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [home, setHome] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [view, setView] = useState<AppView>("chat");
  const [filesOpened, setFilesOpened] = useState(false);
  const filesRef = useRef<FilesHandle>(null);
  const [architectureOpened, setArchitectureOpened] = useState(false);
  const [workspaceTick, setWorkspaceTick] = useState(0);
  // Saving edits changes git status, not the file tree.
  const [savedTick, setSavedTick] = useState(0);
  const chatRef = useRef<ChatHandle>(null);
  const terminalRef = useRef<TerminalHandle>(null);

  const { config, loadFailed, browsingWorkspace, chooseWorkspace, setConfig } =
    useWorkspaceConfig({
      beforeWorkspaceChange: async () => await filesRef.current?.canLeave() ?? true,
      onFirstRun: () => setSettingsOpen(true),
      onWorkspaceChanged: () => {
        setWorkspaceTick((value) => value + 1);
        session.reloadAllowance();
      },
    });

  const session = useAgentSession({
    workspace: config?.workspace ?? "",
    enabled: config !== null,
    onWorkspaceRefresh: () => setWorkspaceTick((value) => value + 1),
  });

  useEffect(() => {
    homeDir().then(setHome).catch(() => setHome(""));
  }, []);

  useEffect(() => {
    if (!config || home === null) return;
    const title = formatWorkdir(config.workspace, home);
    document.title = title;
    getCurrentWindow().setTitle(title).catch(() => {});
  }, [config, home]);

  const { changes, changesError, unstaged } = useGitChanges({
    enabled: config !== null,
    workspace: config?.workspace ?? "",
    refreshToken: workspaceTick + savedTick,
  });

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
      } else if (action === "new-terminal") {
        terminalRef.current?.addTab();
      } else if (action === "history") {
        if (!session.running) setHistoryOpen(true);
      } else {
        session.stop();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  if (!config) {
    return (
      <div className="loading">
        {loadFailed ? "Could not load Oaklet." : "Loading Oaklet..."}
      </div>
    );
  }

  return (
    <div className="app">
      <Sidebar
        view={view}
        onSelectView={selectView}
        unstaged={unstaged}
        allowAll={session.allowAll}
        onRevokeAllowAll={session.revokeAllowAll}
        onChooseWorkspace={chooseWorkspace}
        browsingWorkspace={browsingWorkspace}
        workspace={config.workspace}
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
          running={session.running}
          compacting={session.compacting}
          activeRunId={session.activeRunId}
          context={session.context}
          onSend={session.send}
          onCompact={session.compact}
          onApproval={session.respondToApproval}
          onSendToTerminal={(text) => {
            setView("terminal");
            terminalRef.current?.run(text);
          }}
        />

        <Terminal
          ref={terminalRef}
          key={config.workspace}
          active={view === "terminal"}
          onClose={() => setView("chat")}
        />

        <SourceControl
          key={config.workspace}
          active={view === "git"}
          changes={changes}
          changesError={changesError}
          refreshToken={workspaceTick}
          onRefresh={() => setWorkspaceTick((value) => value + 1)}
        />

        {filesOpened && (
          <Suspense fallback={view === "files" ? <div className="loading">Loading files...</div> : null}>
            <Files
              ref={filesRef}
              key={config.workspace}
              active={view === "files"}
              workspace={config.workspace}
              refreshToken={workspaceTick}
              changes={changes}
              onSaved={() => setSavedTick((value) => value + 1)}
            />
          </Suspense>
        )}

        {architectureOpened && (
          <Suspense
            fallback={
              <div className="arch-loading">Loading architecture...</div>
            }
          >
            <Architecture
              key={config.workspace}
              active={view === "architecture"}
              workspace={config.workspace}
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
          key={config.workspace}
          onClose={() => setSkillsOpen(false)}
        />
      )}
    </div>
  );
}

export default App;
