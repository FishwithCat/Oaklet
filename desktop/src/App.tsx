import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ChatDotsIcon,
  GearIcon,
  GitBranchIcon,
  ShieldCheckIcon,
  TerminalWindowIcon,
  TreeStructureIcon,
} from "@phosphor-icons/react";
import { Chat, type ChatHandle } from "./Chat";
import { SettingsPanel, type AppConfig } from "./SettingsPanel";
import { SourceControl } from "./SourceControl";
import { Terminal } from "./Terminal";
import { useAgentSession } from "./useAgentSession";
import { agentShortcut } from "./shortcuts";
import "./App.css";

const Architecture = lazy(() => import("./Architecture"));

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [view, setView] = useState<
    "chat" | "terminal" | "git" | "architecture"
  >("chat");
  const [architectureOpened, setArchitectureOpened] = useState(false);
  const [unstaged, setUnstaged] = useState<number | null>(null);
  const [workspaceTick, setWorkspaceTick] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const chatRef = useRef<ChatHandle>(null);

  const session = useAgentSession({
    enabled: config !== null,
    onWorkspaceRefresh: () => setWorkspaceTick((value) => value + 1),
  });

  useEffect(() => {
    invoke<AppConfig>("load_config")
      .then((loaded) => {
        setConfig(loaded);
        if (!loaded.apiKey) setSettingsOpen(true);
      })
      .catch(() => setLoadFailed(true));
  }, []);

  useEffect(() => {
    if (!config) return;
    let cancelled = false;
    invoke<{ unstaged: boolean }[]>("git_changes")
      .then((changes) => {
        if (!cancelled) {
          setUnstaged(
            changes.filter((change) => change.unstaged).length,
          );
        }
      })
      .catch(() => {
        if (!cancelled) setUnstaged(null);
      });
    return () => {
      cancelled = true;
    };
  }, [config?.workspace, workspaceTick, view]);

  function newChat() {
    if (session.running) return;
    setView("chat");
    session.reset();
    chatRef.current?.reset();
  }

  function showChat() {
    if (view !== "chat") {
      setView("chat");
      return;
    }
    newChat();
  }

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (settingsOpen) return;
      const action = agentShortcut(event, view, session.running);
      if (!action) return;
      event.preventDefault();
      if (action === "new-session") newChat();
      else session.stop();
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
      <aside className="sidebar">
        <button
          type="button"
          className={`ghost icon-button new-chat${
            view === "chat" ? " active" : ""
          }`}
          onClick={showChat}
          aria-label={view !== "chat" ? "Chat" : "New chat"}
          title={view !== "chat" ? "Chat" : "New chat"}
        >
          <ChatDotsIcon size={18} />
        </button>
        <button
          type="button"
          className={`ghost icon-button terminal-toggle${
            view === "terminal" ? " active" : ""
          }`}
          onClick={() =>
            setView((current) => (current === "terminal" ? "chat" : "terminal"))
          }
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
          onClick={() =>
            setView((current) => (current === "git" ? "chat" : "git"))
          }
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
          onClick={() => {
            setArchitectureOpened(true);
            setView((current) =>
              current === "architecture" ? "chat" : "architecture",
            );
          }}
          aria-label="Architecture"
          aria-pressed={view === "architecture"}
          title="Architecture"
        >
          <TreeStructureIcon size={18} />
        </button>
        <div className="sidebar-foot">
          {session.allowAll && (
            <button
              type="button"
              className="ghost icon-button allow-all-chip"
              onClick={session.revokeAllowAll}
              aria-label="Revoke allowance for all commands"
              title="All commands in this workspace run without approval. Click to ask again."
            >
              <ShieldCheckIcon size={16} />
            </button>
          )}
          <button
            type="button"
            className="ghost icon-button"
            onClick={() => setSettingsOpen(true)}
            aria-label="Settings"
            title="Settings"
          >
            <GearIcon size={16} />
          </button>
        </div>
      </aside>

      <div
        className={`main${
          view === "chat" && session.items.length === 0 ? " empty-state" : ""
        }`}
      >
        <Chat
          ref={chatRef}
          active={view === "chat"}
          items={session.items}
          running={session.running}
          activeRunId={session.activeRunId}
          onSend={session.send}
          onApproval={session.respondToApproval}
        />

        <Terminal
          key={config.workspace}
          active={view === "terminal"}
          onClose={() => setView("chat")}
        />

        <SourceControl
          key={config.workspace}
          active={view === "git"}
          refreshToken={workspaceTick}
          onRefresh={() => setWorkspaceTick((value) => value + 1)}
        />

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
    </div>
  );
}

export default App;
