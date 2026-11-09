import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { message, open } from "@tauri-apps/plugin-dialog";
import {
  ChatDotsIcon,
  FolderOpenIcon,
  GearIcon,
  GitBranchIcon,
  ShieldCheckIcon,
  TerminalWindowIcon,
  TreeStructureIcon,
} from "@phosphor-icons/react";
import { Chat, type ChatHandle } from "./Chat";
import { errorText, SettingsPanel, type AppConfig } from "./SettingsPanel";
import { SessionHistory } from "./SessionHistory";
import { SourceControl, type ChangeInfo } from "./SourceControl";
import { Terminal, type TerminalHandle } from "./Terminal";
import { useAgentSession } from "./useAgentSession";
import { agentShortcut } from "./shortcuts";
import "./App.css";

const Architecture = lazy(() => import("./Architecture"));

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [browsingWorkspace, setBrowsingWorkspace] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [view, setView] = useState<
    "chat" | "terminal" | "git" | "architecture"
  >("chat");
  const [architectureOpened, setArchitectureOpened] = useState(false);
  const [changes, setChanges] = useState<ChangeInfo[] | null>(null);
  const [changesError, setChangesError] = useState("");
  const unstaged = useMemo(
    () => changes?.filter((change) => change.unstaged).length ?? null,
    [changes],
  );
  const [workspaceTick, setWorkspaceTick] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const chatRef = useRef<ChatHandle>(null);
  const terminalRef = useRef<TerminalHandle>(null);

  const session = useAgentSession({
    workspace: config?.workspace ?? "",
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
    setChanges(null);
    setChangesError("");
    invoke<ChangeInfo[]>("git_changes")
      .then((changes) => {
        if (!cancelled) {
          setChanges(changes);
        }
      })
      .catch((error) => {
        if (!cancelled) setChangesError(errorText(error));
      });
    return () => {
      cancelled = true;
    };
  }, [config?.workspace, workspaceTick]);

  function newChat() {
    setView("chat");
    session.newTab();
  }

  function showChat() {
    if (view !== "chat") {
      setView("chat");
      return;
    }
    newChat();
  }

  async function chooseWorkspace() {
    if (!config || browsingWorkspace) return;
    setBrowsingWorkspace(true);
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: config.workspace || undefined,
      });
      if (typeof picked !== "string") return;
      const saved = await invoke<AppConfig>("save_config", {
        config: { ...config, workspace: picked },
      });
      setConfig(saved);
      setWorkspaceTick((value) => value + 1);
      session.reloadAllowance();
    } catch (err) {
      await message(`Could not switch workspace: ${errorText(err)}`, {
        title: "Oaklet",
        kind: "error",
      });
    } finally {
      setBrowsingWorkspace(false);
    }
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
          onClick={() => {
            if (view !== "git") setWorkspaceTick((value) => value + 1);
            setView((current) => (current === "git" ? "chat" : "git"));
          }}
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
            onClick={chooseWorkspace}
            disabled={browsingWorkspace}
            aria-label="Open workspace folder"
            title={`Workspace folder: ${config.workspace}`}
          >
            <FolderOpenIcon size={16} />
          </button>
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
          tabs={session.tabs}
          activeTabId={session.activeTabId}
          onSelectTab={session.selectTab}
          onNewTab={session.newTab}
          onCloseTab={session.closeTab}
          items={session.items}
          running={session.running}
          activeRunId={session.activeRunId}
          onSend={session.send}
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
            chatRef.current?.reset();
            setView("chat");
            setHistoryOpen(false);
          }}
        />
      )}
    </div>
  );
}

export default App;
