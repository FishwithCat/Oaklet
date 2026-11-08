import {
  useEffect,
  useRef,
  useState,
  type SubmitEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ChatDotsIcon,
  FolderOpenIcon,
  GearIcon,
  PaperPlaneRightIcon,
  ShieldCheckIcon,
  StopIcon,
  XIcon,
} from "@phosphor-icons/react";
import { MarkdownText } from "./Markdown";
import { ProcessGroup } from "./Process";
import type {
  ApprovalChoice,
  ProcessItem,
  ToolItem,
} from "./process-model";
import "./App.css";

type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  workspace: string;
};

type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | ProcessItem
  | { kind: "notice"; text: string; tone: "error" | "muted" };

type DeltaEvent = { runId: string; text: string };
type ReasoningEvent = { runId: string; text: string };
type ToolStartEvent = {
  runId: string;
  id: string;
  name: string;
  summary: string;
};
type ToolResultEvent = { runId: string; id: string; ok: boolean; output: string };
type ApprovalEvent = {
  runId: string;
  id: string;
  requestId: string;
  tool: string;
  prompt: string;
  detail: string;
};
type DoneEvent = { runId: string; messages: unknown[] };
type ErrorEvent = { runId: string; message: string };
type RunIdEvent = { runId: string };

const SUGGESTIONS = [
  {
    title: "Map this codebase",
    prompt:
      "Give me a tour of this project: what it does, how it is structured, and where I should start.",
  },
  {
    title: "Fix a bug",
    prompt:
      "Find the most likely bug in this project and fix it. Explain the root cause and how to verify the fix.",
  },
  {
    title: "Add a feature",
    prompt:
      "Add a small, useful feature to this project. Keep the diff minimal and verify it.",
  },
];

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}

function settleStreaming(items: Item[]): Item[] {
  return items.map((item) =>
    (item.kind === "assistant" || item.kind === "reasoning") && item.streaming
      ? { ...item, streaming: false }
      : item,
  );
}

type Block =
  | { kind: "process"; start: number; items: ProcessItem[] }
  | { kind: "single"; index: number; item: Item };

/** Reasoning and tool rows between replies become one compact process group. */
function toBlocks(items: Item[]): Block[] {
  const blocks: Block[] = [];
  let process: ProcessItem[] | null = null;
  let start = 0;

  items.forEach((item, index) => {
    if (item.kind === "reasoning" || item.kind === "tool") {
      if (process === null) {
        process = [];
        start = index;
      }
      process.push(item);
      return;
    }
    if (process !== null) {
      blocks.push({ kind: "process", start, items: process });
      process = null;
    }
    blocks.push({ kind: "single", index, item });
  });

  if (process !== null) {
    blocks.push({ kind: "process", start, items: process });
  }
  return blocks;
}

function SettingsPanel({
  config,
  firstRun,
  onClose,
  onSaved,
}: {
  config: AppConfig;
  firstRun: boolean;
  onClose: () => void;
  onSaved: (config: AppConfig) => void;
}) {
  const [draft, setDraft] = useState(config);
  const [saving, setSaving] = useState(false);
  const [browsing, setBrowsing] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  async function browse() {
    setBrowsing(true);
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: draft.workspace || undefined,
      });
      if (typeof picked === "string") {
        setDraft((current) => ({ ...current, workspace: picked }));
      }
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBrowsing(false);
    }
  }

  async function save(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    setSaving(true);
    setError("");
    try {
      const saved = await invoke<AppConfig>("save_config", { config: draft });
      onSaved(saved);
    } catch (err) {
      setError(errorText(err));
      setSaving(false);
    }
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <form
        className="modal"
        onSubmit={save}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <button
          type="button"
          className="ghost icon-button modal-close"
          onClick={onClose}
          aria-label="Close settings"
          title="Close"
        >
          <XIcon size={16} />
        </button>
        <h2 id="settings-title">{firstRun ? "Connect a model" : "Settings"}</h2>
        <p className="modal-sub">
          Oaklet uses any OpenAI-compatible chat completions API. DeepSeek is
          the default. Your key stays on this machine.
        </p>

        <div className="field">
          <label htmlFor="api-key">API key</label>
          <input
            id="api-key"
            type="password"
            autoComplete="off"
            placeholder="sk-..."
            value={draft.apiKey}
            onChange={(event) =>
              setDraft((current) => ({ ...current, apiKey: event.target.value }))
            }
          />
        </div>

        <div className="field">
          <label htmlFor="base-url">Base URL</label>
          <input
            id="base-url"
            placeholder="https://api.deepseek.com"
            value={draft.baseUrl}
            onChange={(event) =>
              setDraft((current) => ({ ...current, baseUrl: event.target.value }))
            }
          />
        </div>

        <div className="field">
          <label htmlFor="model">Model</label>
          <input
            id="model"
            placeholder="deepseek-flash"
            value={draft.model}
            onChange={(event) =>
              setDraft((current) => ({ ...current, model: event.target.value }))
            }
          />
        </div>

        <div className="field">
          <label htmlFor="workspace">Workspace</label>
          <div className="field-row">
            <input
              id="workspace"
              placeholder="/path/to/project"
              value={draft.workspace}
              onChange={(event) =>
                setDraft((current) => ({
                  ...current,
                  workspace: event.target.value,
                }))
              }
            />
            <button
              type="button"
              className="icon-button"
              onClick={browse}
              disabled={browsing}
              aria-label="Browse"
              title="Browse"
            >
              <FolderOpenIcon size={16} />
            </button>
          </div>
        </div>

        {error && <p className="error-text">{error}</p>}

        <div className="modal-actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" type="submit" disabled={saving}>
            {saving ? "Saving..." : "Save"}
          </button>
        </div>
      </form>
    </div>
  );
}

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [items, setItems] = useState<Item[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [allowAll, setAllowAll] = useState(false);

  const messagesRef = useRef<unknown[]>([]);
  const runIdRef = useRef<string | null>(null);
  const chatRef = useRef<HTMLElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const stickToBottomRef = useRef(true);

  useEffect(() => {
    invoke<AppConfig>("load_config")
      .then((loaded) => {
        setConfig(loaded);
        if (!loaded.apiKey) setSettingsOpen(true);
      })
      .catch((error) => {
        setItems([{ kind: "notice", tone: "error", text: errorText(error) }]);
      });
  }, []);

  useEffect(() => {
    invoke<boolean>("command_allow_all_active")
      .then(setAllowAll)
      .catch(() => setAllowAll(false));
  }, []);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;

    const register = async () => {
      const subscriptions = await Promise.all([
        listen<DeltaEvent>("agent:delta", (event) => {
          const { runId, text } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => {
            const last = previous[previous.length - 1];
            if (last?.kind === "assistant" && last.streaming) {
              return [
                ...previous.slice(0, -1),
                { ...last, text: last.text + text },
              ];
            }
            return [
              ...settleStreaming(previous),
              { kind: "assistant", text, streaming: true },
            ];
          });
        }),
        listen<ReasoningEvent>("agent:reasoning", (event) => {
          const { runId, text } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => {
            const last = previous[previous.length - 1];
            if (
              last?.kind === "reasoning" &&
              last.streaming &&
              last.runId === runId
            ) {
              return [
                ...previous.slice(0, -1),
                { ...last, text: last.text + text },
              ];
            }
            return [
              ...settleStreaming(previous),
              { kind: "reasoning", runId, text, streaming: true },
            ];
          });
        }),
        listen<ToolStartEvent>("agent:tool_start", (event) => {
          const { runId, id, name, summary } = event.payload;
          setActiveRunId(runId);
          setItems((previous) => [
            ...settleStreaming(previous),
            {
              kind: "tool",
              runId,
              id,
              name,
              status: "pending",
              summary,
            },
          ]);
        }),
        listen<ToolResultEvent>("agent:tool_result", (event) => {
          const { id, ok, output } = event.payload;
          setItems((previous) =>
            previous.map((item) =>
              item.kind === "tool" && item.id === id
                ? { ...item, status: ok ? "ok" : "error", output }
                : item,
            ),
          );
        }),
        listen<ApprovalEvent>("agent:approval", (event) => {
          const { id, requestId, prompt, detail } = event.payload;
          setItems((previous) =>
            previous.map((item) =>
              item.kind === "tool" && item.id === id
                ? {
                    ...item,
                    status: "awaiting",
                    requestId,
                    approvalPrompt: prompt,
                    approvalDetail: detail,
                  }
                : item,
            ),
          );
        }),
        listen<DoneEvent>("agent:done", (event) => {
          messagesRef.current = event.payload.messages;
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          setItems(settleStreaming);
        }),
        listen<ErrorEvent>("agent:error", (event) => {
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          setItems((previous) => [
            ...settleStreaming(previous),
            { kind: "notice", tone: "error", text: event.payload.message },
          ]);
        }),
        listen<RunIdEvent>("agent:stopped", () => {
          runIdRef.current = null;
          setActiveRunId(null);
          setRunning(false);
          setItems((previous) => [
            ...settleStreaming(previous),
            { kind: "notice", tone: "muted", text: "Stopped." },
          ]);
        }),
      ]);

      if (disposed) {
        subscriptions.forEach((unlisten) => unlisten());
      } else {
        unlisteners.push(...subscriptions);
      }
    };

    register().catch((error) => {
      setItems((previous) => [
        ...previous,
        { kind: "notice", tone: "error", text: errorText(error) },
      ]);
    });

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (stickToBottomRef.current) {
      endRef.current?.scrollIntoView({ block: "end" });
    }
  }, [items]);

  function onScroll() {
    const element = chatRef.current;
    if (!element) return;
    stickToBottomRef.current =
      element.scrollHeight - element.scrollTop - element.clientHeight < 80;
  }

  function resizeComposer() {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 180)}px`;
  }

  function send() {
    const text = input.trim();
    if (!text || running || !config) return;

    const nextMessages = [
      ...messagesRef.current,
      { role: "user", content: text },
    ];
    messagesRef.current = nextMessages;
    setItems((previous) => [...previous, { kind: "user", text }]);
    setInput("");
    stickToBottomRef.current = true;
    setRunning(true);
    setActiveRunId(null);

    if (textareaRef.current) textareaRef.current.style.height = "auto";

    invoke<string>("agent_send", { messages: nextMessages })
      .then((runId) => {
        runIdRef.current = runId;
      })
      .catch((error) => {
        runIdRef.current = null;
        setRunning(false);
        setItems((previous) => [
          ...previous,
          { kind: "notice", tone: "error", text: errorText(error) },
        ]);
      });
  }

  function stop() {
    if (runIdRef.current) {
      void invoke("agent_stop", { runId: runIdRef.current });
    }
  }

  function newChat() {
    if (running) return;
    messagesRef.current = [];
    runIdRef.current = null;
    setActiveRunId(null);
    setItems([]);
    setInput("");
    textareaRef.current?.focus();
  }

  function respondToApproval(item: ToolItem, choice: ApprovalChoice) {
    if (!item.requestId) return;
    const requestId = item.requestId;
    setItems((previous) =>
      previous.map((entry) =>
        entry.kind === "tool" && entry.id === item.id
          ? { ...entry, status: "running", requestId: undefined }
          : entry,
      ),
    );
    invoke("approve_tool", {
      requestId,
      allow: choice !== "deny",
      always: choice === "allowAll",
    })
      .then(() => {
        if (choice === "allowAll") setAllowAll(true);
      })
      .catch((error) => {
        setItems((previous) => [
          ...previous,
          { kind: "notice", tone: "error", text: errorText(error) },
        ]);
      });
  }

  function revokeAllowAll() {
    invoke("revoke_command_allow_all")
      .then(() => setAllowAll(false))
      .catch((error) => {
        setItems((previous) => [
          ...previous,
          { kind: "notice", tone: "error", text: errorText(error) },
        ]);
      });
  }

  if (!config) {
    return (
      <div className="loading">
        {items.length > 0 ? "Could not load Oaklet." : "Loading Oaklet..."}
      </div>
    );
  }

  const blocks = toBlocks(items);
  let lastProcessBlock = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    if (blocks[index]?.kind === "process") {
      lastProcessBlock = index;
      break;
    }
  }

  return (
    <div className="app">
      <aside className="sidebar">
        <button
          type="button"
          className="ghost icon-button new-chat"
          onClick={newChat}
          disabled={running}
          aria-label="New chat"
          title="New chat"
        >
          <ChatDotsIcon size={18} />
        </button>
        <div className="sidebar-foot">
          {allowAll && (
            <button
              type="button"
              className="ghost icon-button allow-all-chip"
              onClick={revokeAllowAll}
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
        <main className="chat" ref={chatRef} onScroll={onScroll}>
          {items.length === 0 ? (
            <div className="empty">
              <div className="empty-mark">O</div>
              <h1>What should we build?</h1>
              <p>
                Oaklet reads and edits files in{" "}
                <code>{config.workspace}</code> and{" "}
                {allowAll
                  ? "runs commands without asking until you revoke it."
                  : "asks before running any command."}
              </p>
              <div className="suggestions">
                {SUGGESTIONS.map((suggestion) => (
                  <button
                    type="button"
                    key={suggestion.title}
                    className="suggestion"
                    onClick={() => {
                      setInput(suggestion.prompt);
                      requestAnimationFrame(() => {
                        resizeComposer();
                        textareaRef.current?.focus();
                      });
                    }}
                  >
                    <strong>{suggestion.title}</strong>
                    <span>{suggestion.prompt}</span>
                  </button>
                ))}
              </div>
            </div>
          ) : (
            <div className="thread" aria-live="polite">
              {blocks.map((block, blockIndex) => {
                if (block.kind === "process") {
                  return (
                    <ProcessGroup
                      key={`process-${block.start}`}
                      items={block.items}
                      live={
                        running &&
                        blockIndex === lastProcessBlock &&
                        block.items[0]?.runId === activeRunId
                      }
                      onApproval={respondToApproval}
                    />
                  );
                }

                const item = block.item;
                if (item.kind === "user") {
                  return (
                    <div className="msg user" key={`user-${block.index}`}>
                      <div className="bubble">{item.text}</div>
                    </div>
                  );
                }
                if (item.kind === "assistant") {
                  return (
                    <div
                      className="msg assistant"
                      key={`assistant-${block.index}`}
                    >
                      <div className="bubble">
                        <MarkdownText text={item.text} />
                        {item.streaming && <span className="caret" />}
                      </div>
                    </div>
                  );
                }
                if (item.kind !== "notice") return null;
                return (
                  <div
                    className={`notice ${item.tone}`}
                    key={`notice-${block.index}`}
                  >
                    {item.text}
                  </div>
                );
              })}
              <div ref={endRef} />
            </div>
          )}
        </main>

        <div className="composer-wrap">
          <form
            className="composer"
            onSubmit={(event) => {
              event.preventDefault();
              send();
            }}
          >
            <textarea
              ref={textareaRef}
              rows={1}
              value={input}
              placeholder="Ask Oaklet to build, fix, or explain something..."
              onChange={(event) => {
                setInput(event.target.value);
                resizeComposer();
              }}
              onKeyDown={(event) => {
                if (
                  event.key === "Enter" &&
                  !event.shiftKey &&
                  !event.nativeEvent.isComposing
                ) {
                  event.preventDefault();
                  send();
                }
              }}
            />
            <div className="composer-footer">
              <span className="hint">
                Enter to send · Shift+Enter for a new line
              </span>
              {running ? (
                <button
                  type="button"
                  className="stop icon-button"
                  onClick={stop}
                  aria-label="Stop"
                  title="Stop"
                >
                  <StopIcon size={14} weight="fill" />
                </button>
              ) : (
                <button
                  type="submit"
                  className="primary icon-button"
                  disabled={!input.trim()}
                  aria-label="Send"
                  title="Send"
                >
                  <PaperPlaneRightIcon size={16} weight="fill" />
                </button>
              )}
            </div>
          </form>
        </div>
      </div>

      {settingsOpen && (
        <SettingsPanel
          config={config}
          firstRun={!config.apiKey}
          onClose={() => setSettingsOpen(false)}
          onSaved={(saved) => {
            setConfig(saved);
            setSettingsOpen(false);
            invoke<boolean>("command_allow_all_active")
              .then(setAllowAll)
              .catch(() => setAllowAll(false));
          }}
        />
      )}
    </div>
  );
}

export default App;
