import {
  useEffect,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type FormEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import "./App.css";

type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  workspace: string;
};

type ToolStatus = "pending" | "awaiting" | "running" | "ok" | "error";

type ToolItem = {
  kind: "tool";
  id: string;
  name: string;
  args: Record<string, unknown> | null;
  status: ToolStatus;
  output?: string;
  requestId?: string;
};

type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | ToolItem
  | { kind: "notice"; text: string; tone: "error" | "muted" };

type DeltaEvent = { runId: string; text: string };
type ToolStartEvent = { runId: string; id: string; name: string; args: unknown };
type ToolResultEvent = { runId: string; id: string; ok: boolean; output: string };
type ApprovalEvent = {
  runId: string;
  id: string;
  requestId: string;
  tool: string;
  args: Record<string, unknown>;
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

function finalizeStreaming(items: Item[]): Item[] {
  return items.map((item, index) =>
    item.kind === "assistant" && index === items.length - 1
      ? { ...item, streaming: false }
      : item,
  );
}

function MarkdownLink({ href, children }: ComponentPropsWithoutRef<"a">) {
  return (
    <a
      href={href}
      onClick={(event) => {
        event.preventDefault();
        if (href && /^https?:\/\//i.test(href)) {
          void openUrl(href).catch(() => {});
        }
      }}
    >
      {children}
    </a>
  );
}

function MessageText({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ a: MarkdownLink }}>
        {text}
      </ReactMarkdown>
    </div>
  );
}

function toolSummary(item: ToolItem): string {
  const args = item.args ?? {};
  const value =
    item.name === "run_command" ? args.command : args.path;
  return typeof value === "string" ? value : "";
}

function ToolCard({
  item,
  onApproval,
}: {
  item: ToolItem;
  onApproval: (item: ToolItem, allow: boolean) => void;
}) {
  const summary = toolSummary(item);
  return (
    <div className="tool-card">
      <div className="tool-head">
        <span className={`status-dot ${item.status}`} />
        <span className="tool-name">{item.name}</span>
        <span className="tool-summary" title={summary}>
          {summary}
        </span>
      </div>

      {item.status === "awaiting" && item.requestId && (
        <div className="approval">
          <p>Run this command in the workspace?</p>
          <pre>{String(item.args?.command ?? "")}</pre>
          <div className="approval-actions">
            <button
              type="button"
              onClick={() => onApproval(item, false)}
            >
              Deny
            </button>
            <button
              type="button"
              className="primary"
              onClick={() => onApproval(item, true)}
            >
              Allow
            </button>
          </div>
        </div>
      )}

      {item.output && (
        <details open={item.status === "error"}>
          <summary>{item.status === "ok" ? "Output" : "Error"}</summary>
          <pre className="tool-output">{item.output}</pre>
        </details>
      )}
    </div>
  );
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

  async function save(event: FormEvent) {
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
            <button type="button" onClick={browse} disabled={browsing}>
              {browsing ? "..." : "Browse"}
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
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;

    const register = async () => {
      const subscriptions = await Promise.all([
        listen<DeltaEvent>("agent:delta", (event) => {
          setItems((previous) => {
            const last = previous[previous.length - 1];
            if (last?.kind === "assistant" && last.streaming) {
              return [
                ...previous.slice(0, -1),
                { ...last, text: last.text + event.payload.text },
              ];
            }
            return [
              ...previous,
              { kind: "assistant", text: event.payload.text, streaming: true },
            ];
          });
        }),
        listen<ToolStartEvent>("agent:tool_start", (event) => {
          const { id, name, args } = event.payload;
          setItems((previous) => [
            ...finalizeStreaming(previous),
            {
              kind: "tool",
              id,
              name,
              args:
                args && typeof args === "object"
                  ? (args as Record<string, unknown>)
                  : null,
              status: "pending",
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
          const { id, requestId, args } = event.payload;
          setItems((previous) =>
            previous.map((item) =>
              item.kind === "tool" && item.id === id
                ? { ...item, status: "awaiting", requestId, args }
                : item,
            ),
          );
        }),
        listen<DoneEvent>("agent:done", (event) => {
          messagesRef.current = event.payload.messages;
          runIdRef.current = null;
          setRunning(false);
          setItems(finalizeStreaming);
        }),
        listen<ErrorEvent>("agent:error", (event) => {
          runIdRef.current = null;
          setRunning(false);
          setItems((previous) => [
            ...finalizeStreaming(previous),
            { kind: "notice", tone: "error", text: event.payload.message },
          ]);
        }),
        listen<RunIdEvent>("agent:stopped", () => {
          runIdRef.current = null;
          setRunning(false);
          setItems((previous) => [
            ...finalizeStreaming(previous),
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
    setItems([]);
    setInput("");
    textareaRef.current?.focus();
  }

  function respondToApproval(item: ToolItem, allow: boolean) {
    if (!item.requestId) return;
    const requestId = item.requestId;
    setItems((previous) =>
      previous.map((entry) =>
        entry.kind === "tool" && entry.id === item.id
          ? { ...entry, status: "running", requestId: undefined }
          : entry,
      ),
    );
    invoke("approve_tool", { requestId, allow }).catch((error) => {
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

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">O</span>
          <span>Oaklet</span>
          <span className="tag">local agent</span>
        </div>
        <span className="workspace" title={config.workspace}>
          {config.workspace}
        </span>
        <div className="topbar-actions">
          <span className="model">{config.model}</span>
          <button
            type="button"
            className="ghost"
            onClick={newChat}
            disabled={running}
          >
            New chat
          </button>
          <button
            type="button"
            className="ghost"
            onClick={() => setSettingsOpen(true)}
          >
            Settings
          </button>
        </div>
      </header>

      <main className="chat" ref={chatRef} onScroll={onScroll}>
        {items.length === 0 ? (
          <div className="empty">
            <div className="empty-mark">O</div>
            <h1>What should we build?</h1>
            <p>
              Oaklet reads and edits files in{" "}
              <code>{config.workspace}</code> and asks before running any
              command.
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
            {items.map((item, index) => {
              if (item.kind === "user") {
                return (
                  <div className="msg user" key={index}>
                    <div className="bubble">{item.text}</div>
                  </div>
                );
              }
              if (item.kind === "assistant") {
                return (
                  <div className="msg assistant" key={index}>
                    <span className="avatar">O</span>
                    <div className="bubble">
                      <MessageText text={item.text} />
                      {item.streaming && <span className="caret" />}
                    </div>
                  </div>
                );
              }
              if (item.kind === "tool") {
                return (
                  <div className="msg tool" key={index}>
                    <span className="avatar tool-avatar">$</span>
                    <div className="tool-card-wrap">
                      <ToolCard item={item} onApproval={respondToApproval} />
                    </div>
                  </div>
                );
              }
              return (
                <div className={`notice ${item.tone}`} key={index}>
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
              <button type="button" className="stop" onClick={stop}>
                Stop
              </button>
            ) : (
              <button
                type="submit"
                className="primary"
                disabled={!input.trim()}
              >
                Send
              </button>
            )}
          </div>
        </form>
      </div>

      {settingsOpen && (
        <SettingsPanel
          config={config}
          firstRun={!config.apiKey}
          onClose={() => setSettingsOpen(false)}
          onSaved={(saved) => {
            setConfig(saved);
            setSettingsOpen(false);
          }}
        />
      )}
    </div>
  );
}

export default App;
