import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type SubmitEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { CloseRegular } from "@mingcute/react/core-regular";
import { detectPlatform, type ShortcutPlatform } from "../../app/shortcuts";
import { errorText } from "../../shared/error-text";
import { DataSettings } from "./DataSettings";
import { McpSettings } from "./McpSettings";
import {
  formatMcpConfig,
  parseMcpConfig,
  setServerEnabled,
} from "./mcp-settings";
import {
  DEFAULT_SEARCH_URL,
  type AppConfig,
  type ProxyMode,
} from "./config";
import { applyTheme, type Theme } from "./theme";

type SettingsTab =
  | "model"
  | "network"
  | "web"
  | "commands"
  | "mcp"
  | "appearance"
  | "data";

type SettingsDraft = Omit<AppConfig, "mcpServers"> & {
  mcpText: string;
};

type ConnectionTest = {
  model: string;
  latencyMs: number;
};

type TestStatus = {
  ok: boolean;
  text: string;
};

const TABS: { id: SettingsTab; label: string }[] = [
  { id: "model", label: "Model" },
  { id: "network", label: "Network" },
  { id: "web", label: "Web Search" },
  { id: "commands", label: "Commands" },
  { id: "mcp", label: "MCP" },
  { id: "appearance", label: "Appearance" },
  { id: "data", label: "Data" },
];

const SUBTITLES: Record<SettingsTab, string> = {
  model: "Choose the OpenAI-compatible model provider Oaklet should use.",
  network: "Choose how Oaklet reaches the network.",
  web: "Choose the key and endpoint the web_search tool uses.",
  commands: "Choose whether the agent asks before running commands.",
  mcp: "Connect MCP servers and choose which ones the agent can use.",
  appearance: "Choose how Oaklet looks. Auto follows your system appearance.",
  data: "Review the history stored on this machine and delete what you no longer need.",
};

const FIRST_RUN_SUBTITLE =
  "Oaklet uses any OpenAI-compatible chat completions API. DeepSeek is the default. Your key stays on this machine.";

const THEMES: { id: Theme; label: string; hint: string }[] = [
  { id: "light", label: "Light", hint: "Always use the light palette." },
  { id: "dark", label: "Dark", hint: "Always use the dark palette." },
  { id: "auto", label: "Auto", hint: "Follow the system appearance." },
];

function proxyOptions(
  platform: ShortcutPlatform,
): { id: ProxyMode; label: string; hint: string }[] {
  return [
    {
      id: "system",
      label: "System",
      hint:
        platform === "linux"
          ? "Use the proxy in HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, and NO_PROXY, including the values your login shell exports. GNOME and KDE desktop proxy settings are not read."
          : "Use the proxy this machine configures: your shell's proxy variables and the operating system proxy setting.",
    },
    {
      id: "env",
      label: "Environment only",
      hint: "Use only HTTP_PROXY, HTTPS_PROXY, and ALL_PROXY from your environment. Ignores the operating system proxy setting.",
    },
    {
      id: "direct",
      label: "Direct",
      hint: "Never use a proxy. Pick this when the system proxy makes requests slow or unreachable.",
    },
  ];
}

const PROXY_MODES = proxyOptions(detectPlatform());

function toDraft(config: AppConfig): SettingsDraft {
  return {
    ...config,
    mcpText: formatMcpConfig(config.mcpServers ?? {}),
  };
}

export function SettingsPanel({
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
  const [draft, setDraft] = useState(() => toDraft(config));
  const [tab, setTab] = useState<SettingsTab>("model");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [testing, setTesting] = useState(false);
  const [testStatus, setTestStatus] = useState<TestStatus | null>(null);
  const testRequest = useRef(0);
  const mcpParse = useMemo(
    () => parseMcpConfig(draft.mcpText),
    [draft.mcpText],
  );
  const tabRefs = useRef<Record<SettingsTab, HTMLButtonElement | null>>({
    model: null,
    network: null,
    web: null,
    commands: null,
    mcp: null,
    appearance: null,
    data: null,
  });

  useEffect(() => {
    if (saving) return;
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose, saving]);

  useEffect(() => {
    testRequest.current += 1;
    setTesting(false);
    setTestStatus(null);
  }, [draft.baseUrl, draft.apiKey, draft.model, draft.proxyMode]);

  // Theme changes preview immediately; leaving the panel restores the saved one.
  useEffect(() => {
    applyTheme(draft.theme);
    return () => applyTheme(config.theme);
  }, [draft.theme, config.theme]);

  function onTabKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    const index = TABS.findIndex((item) => item.id === tab);
    let next = index;
    if (event.key === "ArrowDown" || event.key === "ArrowRight") {
      next = (index + 1) % TABS.length;
    } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
      next = (index - 1 + TABS.length) % TABS.length;
    } else if (event.key === "Home") {
      next = 0;
    } else if (event.key === "End") {
      next = TABS.length - 1;
    } else {
      return;
    }
    event.preventDefault();
    const target = TABS[next];
    if (!target) return;
    setTab(target.id);
    tabRefs.current[target.id]?.focus();
  }

  async function save(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    const parsed = parseMcpConfig(draft.mcpText);
    if ("error" in parsed) {
      setTab("mcp");
      setError("");
      return;
    }
    setSaving(true);
    setError("");
    try {
      const { mcpText: _mcpText, ...rest } = draft;
      const saved = await invoke<AppConfig>("save_config", {
        config: { ...rest, mcpServers: parsed.servers },
      });
      onSaved(saved);
    } catch (err) {
      setError(errorText(err));
      setSaving(false);
    }
  }

  async function testConnection() {
    const request = ++testRequest.current;
    setTesting(true);
    setTestStatus(null);
    try {
      const result = await invoke<ConnectionTest>("test_connection", {
        baseUrl: draft.baseUrl,
        apiKey: draft.apiKey,
        model: draft.model,
        proxyMode: draft.proxyMode,
      });
      if (request !== testRequest.current) return;
      setTestStatus({
        ok: true,
        text: `Connected. ${result.model} replied in ${result.latencyMs} ms.`,
      });
    } catch (err) {
      if (request !== testRequest.current) return;
      setTestStatus({ ok: false, text: errorText(err) });
    } finally {
      if (request === testRequest.current) setTesting(false);
    }
  }

  function close() {
    if (!saving) onClose();
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <form
        className="modal settings-modal"
        onSubmit={save}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <button
          type="button"
          className="ghost icon-button modal-close"
          onClick={close}
          disabled={saving}
          aria-label="Close settings"
          title="Close"
        >
          <CloseRegular size={16} />
        </button>

        <div className="settings-head">
          <h2 id="settings-title">
            {firstRun && tab === "model" ? "Connect a model" : "Settings"}
          </h2>
          <p className="modal-sub">
            {firstRun && tab === "model" ? FIRST_RUN_SUBTITLE : SUBTITLES[tab]}
          </p>
        </div>

        <div className="settings-layout">
          <div
            className="settings-tablist"
            role="tablist"
            aria-orientation="vertical"
            aria-label="Settings sections"
          >
            {TABS.map((item) => (
              <button
                key={item.id}
                ref={(node) => {
                  tabRefs.current[item.id] = node;
                }}
                type="button"
                role="tab"
                id={`settings-tab-${item.id}`}
                aria-selected={tab === item.id}
                aria-controls={`settings-panel-${item.id}`}
                tabIndex={tab === item.id ? 0 : -1}
                className={`settings-tab${tab === item.id ? " active" : ""}`}
                onClick={() => setTab(item.id)}
                onKeyDown={onTabKeyDown}
              >
                {item.label}
              </button>
            ))}
          </div>

          <div className="settings-content">
            {tab === "data" ? (
              <section
                id="settings-panel-data"
                role="tabpanel"
                aria-labelledby="settings-tab-data"
                tabIndex={0}
              >
                <DataSettings />
              </section>
            ) : tab === "model" ? (
              <section
                id="settings-panel-model"
                role="tabpanel"
                aria-labelledby="settings-tab-model"
                tabIndex={0}
              >
                <div className="field">
                  <label htmlFor="api-key">API key</label>
                  <input
                    id="api-key"
                    type="password"
                    autoComplete="off"
                    placeholder="sk-..."
                    value={draft.apiKey}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        apiKey: event.target.value,
                      }))
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
                      setDraft((current) => ({
                        ...current,
                        baseUrl: event.target.value,
                      }))
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
                      setDraft((current) => ({
                        ...current,
                        model: event.target.value,
                      }))
                    }
                  />
                </div>

                <div className="settings-test">
                  {testStatus && (
                    <p
                      className={`settings-test-status${
                        testStatus.ok ? " ok" : " error"
                      }`}
                      role={testStatus.ok ? "status" : "alert"}
                    >
                      {testStatus.text}
                    </p>
                  )}
                  <button
                    type="button"
                    className="primary"
                    onClick={testConnection}
                    aria-label="Test connection"
                    disabled={
                      testing ||
                      saving ||
                      !draft.baseUrl.trim() ||
                      !draft.model.trim()
                    }
                  >
                    {testing ? "Testing..." : "Test"}
                  </button>
                </div>

                <div className="field">
                  <label htmlFor="summary-model">Summary model</label>
                  <input
                    id="summary-model"
                    placeholder={draft.model}
                    value={draft.summaryModel}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        summaryModel: event.target.value,
                      }))
                    }
                  />
                  <p className="field-hint">
                    Optional. Empty reuses Model, so compaction summaries hit
                    the provider's prefix cache. A cheaper model lowers the
                    per-token price but loses that cache.
                  </p>
                </div>

                {draft.summaryModel.trim() && draft.summaryModel.trim() !== draft.model.trim() && (
                  <div className="field">
                    <label htmlFor="summary-context-window">Summary context window</label>
                    <input
                      id="summary-context-window"
                      type="number"
                      min={8192}
                      step={1024}
                      value={draft.summaryContextWindow ?? ""}
                      onChange={(event) => setDraft((current) => ({
                        ...current,
                        summaryContextWindow: event.target.value ? Number(event.target.value) : null,
                      }))}
                    />
                    <p className="field-hint">Required for a separate summary model. Include its input and output capacity.</p>
                  </div>
                )}

                <div className="field">
                  <label htmlFor="context-window">Context window</label>
                  <input
                    id="context-window"
                    type="number"
                    min={8192}
                    step={1024}
                    value={draft.contextWindow}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        contextWindow: Number(event.target.value),
                      }))
                    }
                  />
                  <p className="field-hint">
                    Prompt tokens the model accepts. Oaklet summarizes older
                    history near 80% of this.
                  </p>
                </div>
              </section>
            ) : tab === "web" ? (
              <section
                id="settings-panel-web"
                role="tabpanel"
                aria-labelledby="settings-tab-web"
                tabIndex={0}
              >
                <div className="field">
                  <label htmlFor="search-url">Search URL</label>
                  <input
                    id="search-url"
                    placeholder={DEFAULT_SEARCH_URL}
                    value={draft.searchUrl}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        searchUrl: event.target.value,
                      }))
                    }
                  />
                  <p className="field-hint">
                    Used by web_search. Empty defaults to the DeepSeek Search
                    API. The endpoint must speak the DeepSeek search API.
                  </p>
                </div>

                <div className="field">
                  <label htmlFor="search-api-key">Search API key</label>
                  <input
                    id="search-api-key"
                    type="password"
                    autoComplete="off"
                    placeholder="Optional"
                    value={draft.searchApiKey}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        searchApiKey: event.target.value,
                      }))
                    }
                  />
                  <p className="field-hint">
                    Optional. Empty reuses the model API key.
                  </p>
                </div>
              </section>
            ) : tab === "commands" ? (
              <section
                id="settings-panel-commands"
                role="tabpanel"
                aria-labelledby="settings-tab-commands"
                tabIndex={0}
              >
                <label className="settings-check">
                  <input
                    type="checkbox"
                    checked={draft.allowAllCommands}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        allowAllCommands: event.target.checked,
                      }))
                    }
                  />
                  <span>
                    <strong>Run commands without approval</strong>
                    <span className="field-hint">
                      The agent runs run_command in this workspace without an
                      approval card. The sidebar toggle grants or revokes it for
                      this session. File tools never ask for approval, so this
                      setting does not cover them.
                    </span>
                  </span>
                </label>
                <label className="settings-check">
                  <input type="checkbox" checked={draft.keepWorktreesOnArchive}
                    onChange={(event) => setDraft((current) => ({
                      ...current, keepWorktreesOnArchive: event.target.checked,
                    }))} />
                  <span>
                    <strong>Keep worktrees when archiving</strong>
                    <span className="field-hint">Keep the checkout and its files after archiving a chat.</span>
                  </span>
                </label>
              </section>
            ) : tab === "mcp" ? (
              <section
                id="settings-panel-mcp"
                role="tabpanel"
                aria-labelledby="settings-tab-mcp"
                tabIndex={0}
              >
                <McpSettings
                  text={draft.mcpText}
                  parsed={mcpParse}
                  onChange={(mcpText) =>
                    setDraft((current) => ({ ...current, mcpText }))
                  }
                  onToggleServer={(name, enabled) =>
                    setDraft((current) => {
                      const parsed = parseMcpConfig(current.mcpText);
                      if ("error" in parsed) return current;
                      return {
                        ...current,
                        mcpText: formatMcpConfig(
                          setServerEnabled(parsed.servers, name, enabled),
                        ),
                      };
                    })
                  }
                />
              </section>
            ) : tab === "network" ? (
              <section
                id="settings-panel-network"
                role="tabpanel"
                aria-labelledby="settings-tab-network"
                tabIndex={0}
              >
                <div role="radiogroup" aria-label="Proxy">
                  {PROXY_MODES.map((option) => (
                    <label key={option.id} className="settings-check">
                      <input
                        type="radio"
                        name="proxy-mode"
                        value={option.id}
                        checked={draft.proxyMode === option.id}
                        onChange={() =>
                          setDraft((current) => ({
                            ...current,
                            proxyMode: option.id,
                          }))
                        }
                      />
                      <span>
                        <strong>{option.label}</strong>
                        <span className="field-hint">{option.hint}</span>
                      </span>
                    </label>
                  ))}
                </div>
                <p className="field-hint">
                  Model, web search, web fetch, and MCP requests all use this
                  choice. The Test button in the Model tab measures the
                  connection with the choice above before you save.
                </p>
              </section>
            ) : (
              <section
                id="settings-panel-appearance"
                role="tabpanel"
                aria-labelledby="settings-tab-appearance"
                tabIndex={0}
              >
                <div role="radiogroup" aria-label="Theme">
                  {THEMES.map((option) => (
                    <label key={option.id} className="settings-check">
                      <input
                        type="radio"
                        name="theme"
                        value={option.id}
                        checked={draft.theme === option.id}
                        onChange={() =>
                          setDraft((current) => ({
                            ...current,
                            theme: option.id,
                          }))
                        }
                      />
                      <span>
                        <strong>{option.label}</strong>
                        <span className="field-hint">{option.hint}</span>
                      </span>
                    </label>
                  ))}
                </div>
              </section>
            )}
          </div>
        </div>

        {error && <p className="error-text settings-error">{error}</p>}

        <div className="modal-actions settings-actions">
          {tab === "data" ? (
            <button type="button" onClick={close} disabled={saving}>
              Close
            </button>
          ) : (
            <>
              <button type="button" onClick={close} disabled={saving}>
                Cancel
              </button>
              <button className="primary" type="submit" disabled={saving}>
                {saving ? "Saving..." : "Save"}
              </button>
            </>
          )}
        </div>
      </form>
    </div>
  );
}
