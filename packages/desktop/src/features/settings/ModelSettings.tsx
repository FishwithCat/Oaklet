import type { AppConfig } from "./config";

type ModelFields = Pick<
  AppConfig,
  "apiKey" | "baseUrl" | "model" | "summaryModel" | "summaryContextWindow" | "contextWindow"
>;

export type TestStatus = {
  ok: boolean;
  text: string;
};

export function ModelSettings({
  draft,
  onChange,
  saving,
  testing,
  testStatus,
  testConnection,
}: {
  draft: ModelFields;
  onChange: (changes: Partial<ModelFields>) => void;
  saving: boolean;
  testing: boolean;
  testStatus: TestStatus | null;
  testConnection: () => void;
}) {
  return (
    <>
      <div className="field">
        <label htmlFor="api-key">API key</label>
        <input
          id="api-key"
          type="password"
          autoComplete="off"
          placeholder="sk-..."
          value={draft.apiKey}
          onChange={(event) => onChange({ apiKey: event.target.value })}
        />
      </div>

      <div className="field">
        <label htmlFor="base-url">Base URL</label>
        <input
          id="base-url"
          placeholder="https://api.deepseek.com"
          value={draft.baseUrl}
          onChange={(event) => onChange({ baseUrl: event.target.value })}
        />
      </div>

      <div className="field">
        <label htmlFor="model">Model</label>
        <input
          id="model"
          placeholder="deepseek-flash"
          value={draft.model}
          onChange={(event) => onChange({ model: event.target.value })}
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
          onChange={(event) => onChange({ summaryModel: event.target.value })}
        />
        <p className="field-hint">
          Optional. Empty reuses Model, so compaction summaries hit
          the provider's prefix cache. A cheaper model lowers the
          per-token price but loses that cache.
        </p>
      </div>

      {draft.summaryModel.trim() &&
        draft.summaryModel.trim() !== draft.model.trim() && (
          <div className="field">
            <label htmlFor="summary-context-window">Summary context window</label>
            <input
              id="summary-context-window"
              type="number"
              min={8192}
              step={1024}
              value={draft.summaryContextWindow ?? ""}
              onChange={(event) =>
                onChange({
                  summaryContextWindow: event.target.value
                    ? Number(event.target.value)
                    : null,
                })
              }
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
          onChange={(event) => onChange({ contextWindow: Number(event.target.value) })}
        />
        <p className="field-hint">
          Prompt tokens the model accepts. Oaklet summarizes older
          history near 80% of this.
        </p>
      </div>
    </>
  );
}
