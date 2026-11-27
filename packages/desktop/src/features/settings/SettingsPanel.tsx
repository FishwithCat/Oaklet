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
import { errorText } from "../../shared/error-text";
import { DataSettings } from "./DataSettings";
import { McpSettings } from "./McpSettings";
import { ModelSettings, type TestStatus } from "./ModelSettings";
import {
  AppearanceSettings,
  CommandSettings,
  NetworkSettings,
  WebSearchSettings,
} from "./SettingsFields";
import {
  formatMcpConfig,
  parseMcpConfig,
  setServerEnabled,
} from "./mcp-settings";
import type { AppConfig } from "./config";
import { applyTheme } from "./theme";

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

  function updateDraft(changes: Partial<SettingsDraft>) {
    setDraft((current) => ({ ...current, ...changes }));
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
            <section
              id={`settings-panel-${tab}`}
              role="tabpanel"
              aria-labelledby={`settings-tab-${tab}`}
              tabIndex={0}
            >
              {tab === "data" ? (
                <DataSettings />
              ) : tab === "model" ? (
                <ModelSettings
                  draft={draft}
                  onChange={updateDraft}
                  saving={saving}
                  testing={testing}
                  testStatus={testStatus}
                  testConnection={testConnection}
                />
              ) : tab === "web" ? (
                <WebSearchSettings draft={draft} onChange={updateDraft} />
              ) : tab === "commands" ? (
                <CommandSettings draft={draft} onChange={updateDraft} />
              ) : tab === "mcp" ? (
                <McpSettings
                  text={draft.mcpText}
                  parsed={mcpParse}
                  onChange={(mcpText) => updateDraft({ mcpText })}
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
              ) : tab === "network" ? (
                <NetworkSettings
                  value={draft.proxyMode}
                  onChange={(proxyMode) => updateDraft({ proxyMode })}
                />
              ) : (
                <AppearanceSettings
                  value={draft.theme}
                  onChange={(theme) => updateDraft({ theme })}
                />
              )}
            </section>
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
