import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { errorText } from "../../shared/error-text";
import type { CodeIndexConfig } from "./config";
import { parseIndexArgs, stateText, subscribeIndex, updateIndexServer, type IndexLanguage, type IndexProject, type IndexStatus } from "./code-index-model";

const LANGUAGES: { id: IndexLanguage; name: string; command: string }[] = [
  { id: "rust", name: "Rust", command: "rust-analyzer" },
  { id: "typescript", name: "TypeScript / JavaScript", command: "typescript-language-server" },
  { id: "python", name: "Python", command: "pyright-langserver" },
  { id: "go", name: "Go", command: "gopls" },
  { id: "java", name: "Java", command: "jdtls" },
];

function ServerFields({ language, config, disabled, onChange, onValidity }: {
  language: typeof LANGUAGES[number]; config: CodeIndexConfig; disabled: boolean;
  onChange: (config: CodeIndexConfig) => void; onValidity: (language: IndexLanguage, error: string) => void;
}) {
  const server = config.servers[language.id] ?? {};
  const [args, setArgs] = useState(() => JSON.stringify(server.args ?? []));
  const [error, setError] = useState("");
  function editArgs(text: string) {
    setArgs(text);
    try {
      const parsed = parseIndexArgs(text);
      onChange(updateIndexServer(config, language.id, { args: parsed }));
      setError("");
      onValidity(language.id, "");
    } catch (reason) {
      const detail = errorText(reason);
      setError(detail);
      onValidity(language.id, detail);
    }
  }
  return (
    <fieldset className="index-server" disabled={disabled}>
      <legend className="sr-only">{language.name}</legend>
      <div className="index-heading">
        <h4>{language.name}</h4>
        <label className="index-enable">
          <input type="checkbox" checked={server.enabled !== false}
            onChange={(event) => onChange(updateIndexServer(config, language.id, { enabled: event.target.checked }))} />
          Enable indexing
        </label>
      </div>
      <details className="index-server-options" open={!!error || !!server.command || !!server.args?.length || undefined}>
        <summary>Server configuration</summary>
        <div className="field">
          <label htmlFor={`index-command-${language.id}`}>Server executable</label>
          <input id={`index-command-${language.id}`} placeholder={`${language.command} (PATH)`} value={server.command ?? ""}
            onChange={(event) => onChange(updateIndexServer(config, language.id, { command: event.target.value || null }))} />
          <p className="field-hint">Leave empty to detect from PATH. No shell is used.</p>
        </div>
        <div className="field">
          <label htmlFor={`index-args-${language.id}`}>Arguments (JSON array)</label>
          <input id={`index-args-${language.id}`} value={args} aria-invalid={!!error} aria-describedby={`index-args-hint-${language.id}`} onChange={(event) => editArgs(event.target.value)} />
          <p id={`index-args-hint-${language.id}`} className={error ? "error-text" : "field-hint"}>
            {error || (language.id === "typescript" || language.id === "python"
              ? 'Custom arguments require an executable. Default: ["--stdio"].'
              : 'Custom arguments require an executable. Default: [].')}
          </p>
        </div>
      </details>
    </fieldset>
  );
}

export function CodeIndexSettings({ config, disabled, onChange, onValidity }: {
  config: CodeIndexConfig; disabled: boolean; onChange: (config: CodeIndexConfig) => void;
  onValidity: (language: IndexLanguage, error: string) => void;
}) {
  const [status, setStatus] = useState<IndexStatus | null>(null);
  const [error, setError] = useState("");
  const [streamError, setStreamError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => subscribeIndex(
    (receive) => getCurrentWindow().listen<IndexStatus>("code-index://changed", (event) => receive(event.payload)),
    () => invoke<IndexStatus>("code_index_status"),
    (next) => { setStatus(next); setStreamError(""); },
    (reason) => setStreamError(errorText(reason)),
  ), []);
  async function act(command: "code_index_retry" | "code_index_rebuild", project?: IndexProject) {
    setBusy(true);
    setError("");
    try { await invoke(command, project ? { projectRoot: project.root, language: project.language } : {}); }
    catch (reason) { setError(errorText(reason)); }
    finally { setBusy(false); }
  }
  return (
    <div className="code-index-settings">
      <div className="index-heading"><h3>Current workspace</h3></div>
      {!status && !error && !streamError && <p className="field-hint" role="status">Loading index status…</p>}
      {(error || streamError) && <p className="error-text" role="alert">{error || streamError}</p>}
      {status && <WorkspaceIndexStatus status={status} disabled={disabled || busy} busy={busy}
        onRetry={(project) => void act("code_index_retry", project)}
        onRebuild={() => void act("code_index_rebuild")} />}
      <section className="index-servers" aria-labelledby="index-servers-title">
        <div className="index-heading"><h3 id="index-servers-title">Language servers</h3><span className="field-hint">Global settings</span></div>
        <p className="field-hint">Servers must be installed separately. Save applies server settings to every open workspace.</p>
        <div className="index-server-list">
          {LANGUAGES.map((language) => <ServerFields key={language.id} language={language} config={config} disabled={disabled} onChange={onChange} onValidity={onValidity} />)}
        </div>
        <details className="index-requirements">
          <summary>Required runtimes</summary>
          <p className="field-hint">Rust: rust-analyzer and a Rust toolchain. TypeScript / JavaScript: Node.js, typescript-language-server and a TypeScript SDK.</p>
          <p className="field-hint">Python: Node.js, Pyright and a Python interpreter. Go: gopls and a local Go toolchain. Java: JDT LS, JDK 21+ and Python 3 for the default jdtls launcher. Project dependencies must already be available locally.</p>
        </details>
      </section>
    </div>
  );
}

export function WorkspaceIndexStatus({ status, disabled, busy, onRetry, onRebuild }: {
  status: IndexStatus; disabled: boolean; busy: boolean;
  onRetry: (project: IndexProject) => void; onRebuild: () => void;
}) {
  const disk = status.persistence;
  return (
    <div className="index-workspace" aria-live="polite">
      <p className="index-path">{status.workspace}</p>
      <p className="index-summary">
        {status.discovering ? "Discovering projects…" : status.stale ? "Reconciling saved files…" : "Saved-file watcher active"}
        {status.incomplete ? " · Project discovery incomplete" : ""}
      </p>
      {status.warnings.map((warning) => <p key={warning} className="error-text">{warning}</p>)}
      {disk ? <>
        <div className="index-heading">
          <h4>Disk index</h4>
          <span className="field-hint">{disk.verifying ? "Verifying…" : "Verification settled"}{disk.extracting ? " · Extracting symbols…" : ""}</span>
        </div>
        <dl className="index-metrics">
          <div><dt>Files checked</dt><dd>{disk.files} / {disk.total_files}</dd></div>
          <div><dt>With symbols</dt><dd>{disk.extracted_files}</dd></div>
          <div><dt>Reused</dt><dd>{disk.reused_files}</dd></div>
          <div><dt>Skipped</dt><dd>{disk.skipped_files}</dd></div>
          <div><dt>Projects restored</dt><dd>{disk.restored_projects}</dd></div>
          <div><dt>Memory cache</dt><dd>{status.cache.entries} <span className="field-hint">/ {status.cache.bytes} B</span></dd></div>
        </dl>
        {disk.error && <p className="error-text">{disk.error}</p>}
      </> : <p className="field-hint">Persistent index unavailable. Memory cache: {status.cache.entries} entries ({status.cache.bytes} bytes).</p>}
      {status.cache.error && <p className="error-text">{status.cache.error}</p>}
      {status.projects.length === 0 && !status.discovering && <p className="field-hint">No supported code projects found.</p>}
      <div className="index-projects">
        {status.projects.map((project) => (
          <article className="index-project" key={`${project.language}:${project.root}`}>
            <div className="index-heading">
              <h4>{LANGUAGES.find((language) => language.id === project.language)?.name ?? project.language}</h4>
              <button type="button" disabled={disabled || project.state === "Disabled"} onClick={() => onRetry(project)} title="Retry this project's saved configuration">Retry</button>
            </div>
            <p className="index-path">{project.root}</p>
            <p className={typeof project.state === "object" ? "error-text" : "index-summary"}>{stateText(project.state)}</p>
            <dl className="index-metadata">
              {disk && <div><dt>Disk snapshot</dt><dd>{disk.projects.some((item) => item.root === project.root && item.language === project.language && item.verified) ? "File bytes verified" : "Unverified"}</dd></div>}
              {project.version && <div><dt>Server</dt><dd>{project.version}</dd></div>}
              {project.capabilities && <div><dt>Capabilities</dt><dd>{Object.entries(project.capabilities).filter(([, supported]) => supported).map(([name]) => name.replace(/_/g, " ")).join(", ") || "None"}</dd></div>}
            </dl>
            {project.progress.map((progress, index) => <p className="field-hint" key={index}>{progress}</p>)}
          </article>
        ))}
      </div>
      <p className="field-hint">Verified file symbols may be reused from SQLite or memory. Definitions, references and calls use live queries. Semantic completeness remains unknown.</p>
      <div className="index-rebuild">
        <button type="button" disabled={disabled} onClick={onRebuild}>{busy ? "Working…" : "Clear and rebuild index"}</button>
        <p className="field-hint">Clears this workspace's index and query cache. Source files and chat history are preserved.</p>
      </div>
    </div>
  );
}
