import { detectPlatform, type ShortcutPlatform } from "../../app/shortcuts";
import { DEFAULT_SEARCH_URL, type AppConfig, type ProxyMode } from "./config";
import type { Theme } from "./theme";

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

type SearchFields = Pick<AppConfig, "searchUrl" | "searchApiKey">;

export function WebSearchSettings({ draft, onChange }: {
  draft: SearchFields;
  onChange: (changes: Partial<SearchFields>) => void;
}) {
  return (
    <>
      <div className="field">
        <label htmlFor="search-url">Search URL</label>
        <input
          id="search-url"
          placeholder={DEFAULT_SEARCH_URL}
          value={draft.searchUrl}
          onChange={(event) => onChange({ searchUrl: event.target.value })}
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
          onChange={(event) => onChange({ searchApiKey: event.target.value })}
        />
        <p className="field-hint">
          Optional. Empty reuses the model API key.
        </p>
      </div>
    </>
  );
}

type CommandFields = Pick<AppConfig, "allowAllCommands" | "keepWorktreesOnArchive">;

export function CommandSettings({ draft, onChange }: {
  draft: CommandFields;
  onChange: (changes: Partial<CommandFields>) => void;
}) {
  return (
    <>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={draft.allowAllCommands}
          onChange={(event) => onChange({ allowAllCommands: event.target.checked })}
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
        <input
          type="checkbox"
          checked={draft.keepWorktreesOnArchive}
          onChange={(event) =>
            onChange({ keepWorktreesOnArchive: event.target.checked })
          }
        />
        <span>
          <strong>Keep worktrees when archiving</strong>
          <span className="field-hint">Keep the checkout and its files after archiving a chat.</span>
        </span>
      </label>
    </>
  );
}

export function NetworkSettings({ value, onChange }: {
  value: ProxyMode;
  onChange: (value: ProxyMode) => void;
}) {
  return (
    <>
      <p className="field-hint network-intro">
        Model, web search, web fetch, and MCP requests all use this
        choice. The Test button in the Model tab measures the
        connection with the choice below before you save.
      </p>
      <div role="radiogroup" aria-label="Proxy">
        {PROXY_MODES.map((option) => (
          <label key={option.id} className="settings-check">
            <input
              type="radio"
              name="proxy-mode"
              value={option.id}
              checked={value === option.id}
              onChange={() => onChange(option.id)}
            />
            <span>
              <strong>{option.label}</strong>
              <span className="field-hint">{option.hint}</span>
            </span>
          </label>
        ))}
      </div>
    </>
  );
}

export function AppearanceSettings({ value, onChange }: {
  value: Theme;
  onChange: (value: Theme) => void;
}) {
  return (
    <div role="radiogroup" aria-label="Theme">
      {THEMES.map((option) => (
        <label key={option.id} className="settings-check">
          <input
            type="radio"
            name="theme"
            value={option.id}
            checked={value === option.id}
            onChange={() => onChange(option.id)}
          />
          <span>
            <strong>{option.label}</strong>
            <span className="field-hint">{option.hint}</span>
          </span>
        </label>
      ))}
    </div>
  );
}
