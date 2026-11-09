import { useEffect, useState, type SubmitEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpenIcon, XIcon } from "@phosphor-icons/react";

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  workspace: string;
};

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
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
