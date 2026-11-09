import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ChatDotsIcon,
  GearIcon,
  GitBranchIcon,
  ShieldCheckIcon,
  TerminalWindowIcon,
} from "@phosphor-icons/react";
import { Chat, type ChatHandle } from "./Chat";
import { SettingsPanel, type AppConfig } from "./SettingsPanel";
import { SourceControl } from "./SourceControl";
import { Terminal } from "./Terminal";
import { useAgentSession } from "./useAgentSession";
import "./App.css";

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [view, setView] = useState<"chat" | "terminal" | "git">("chat");
  const [ahead, setAhead] = useState<number | null>(null);
  const [gitTick, setGitTick] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const chatRef = useRef<ChatHandle>(null);

  const session = useAgentSession({
    enabled: config !== null,
    onGitRefresh: () => setGitTick((value) => value + 1),
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
    invoke<number | null>("git_ahead")
      .then((count) => {
        if (!cancelled) setAhead(count);
      })
      .catch(() => {
        if (!cancelled) setAhead(null);
      });
    return () => {
      cancelled = true;
    };
  }, [config?.workspace, gitTick, view]);

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
            ahead
              ? `Source control (${ahead} unpushed)`
              : "Source control"
          }
          aria-pressed={view === "git"}
          title={
            ahead
              ? `${ahead} commit${ahead === 1 ? "" : "s"} not pushed`
              : "Source control"
          }
        >
          <GitBranchIcon size={18} />
          {ahead !== null && ahead > 0 && (
            <span className="sc-badge" aria-hidden="true">
              {ahead > 99 ? "99+" : ahead}
            </span>
          )}
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

      <div className="main">
        <Chat
          ref={chatRef}
          active={view === "chat"}
          items={session.items}
          running={session.running}
          activeRunId={session.activeRunId}
          workspace={config.workspace}
          allowAll={session.allowAll}
          onSend={session.send}
          onStop={session.stop}
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
          refreshToken={gitTick}
          onRefresh={() => setGitTick((value) => value + 1)}
        />
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
