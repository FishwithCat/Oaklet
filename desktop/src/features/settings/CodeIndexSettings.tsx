import { useEffect, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { errorText } from "../../shared/error-text";
import type { CodeIndexConfig } from "./config";
import { parseIndexArgs, stateText, subscribeIndex, updateIndexServer, type IndexInstall, type IndexLanguage, type IndexProject, type IndexStatus } from "./code-index-model";

const LANGUAGES: { id: IndexLanguage; name: string; command: string }[] = [
  { id: "rust", name: "Rust", command: "rust-analyzer" },
  { id: "typescript", name: "TypeScript / JavaScript", command: "typescript-language-server" },
  { id: "python", name: "Python", command: "pyright-langserver" },
  { id: "go", name: "Go", command: "gopls" },
  { id: "java", name: "Java", command: "jdtls" },
];

const cardStyle: CSSProperties = { minWidth: 0, margin: 0, padding: 12, border: "1px solid var(--border)", borderRadius: 8 };
const srOnlyStyle: CSSProperties = {
  position: "absolute", width: 1, height: 1, padding: 0, margin: -1,
  overflow: "hidden", clip: "rect(0, 0, 0, 0)", whiteSpace: "nowrap", border: 0,
};
const headingStyle: CSSProperties = {
  display: "flex", flexWrap: "wrap", alignItems: "center", justifyContent: "space-between", gap: "6px 12px", marginBottom: 8,
};
const titleStyle: CSSProperties = { margin: 0, fontSize: 12, fontWeight: 600 };
const toggleStyle: CSSProperties = { display: "inline-flex", alignItems: "center", gap: 6, color: "var(--muted)", fontSize: 11, cursor: "pointer" };
const summaryStyle: CSSProperties = { width: "fit-content", color: "var(--muted)", fontSize: 11, cursor: "pointer" };
const fieldStyle: CSSProperties = {
  display: "grid", gridTemplateColumns: "minmax(96px, 140px) minmax(0, 1fr)", alignItems: "center", columnGap: 12, rowGap: 4,
};
const labelStyle: CSSProperties = { fontSize: 11.5, fontWeight: 600, color: "var(--muted)", letterSpacing: "0.2px" };
const inputStyle: CSSProperties = {
  width: "100%", minWidth: 0, background: "var(--bg)", border: "1px solid var(--border)", borderRadius: 7,
  padding: "7px 9px", fontSize: 12, fontFamily: "var(--mono)",
};
const hintStyle: CSSProperties = { margin: 0, color: "var(--muted)", fontSize: 11, lineHeight: 1.5, gridColumn: 2 };
const errorStyle: CSSProperties = { margin: 0, color: "var(--red)", fontSize: 11.5, whiteSpace: "pre-wrap" };
const buttonStyle: CSSProperties = { padding: "5px 10px", fontSize: 12 };

export function ServerFields({ language, config, disabled, busy, projects, installation, onChange, onValidity, onRetry, onInstall, onCancelInstall }: {
  language: typeof LANGUAGES[number]; config: CodeIndexConfig; disabled: boolean;
  busy: boolean; projects: IndexProject[]; installation?: IndexInstall;
  onChange: (config: CodeIndexConfig) => void; onValidity: (language: IndexLanguage, error: string) => void;
  onRetry: (projects: IndexProject[]) => void; onInstall: (project: IndexProject) => void;
  onCancelInstall: () => void;
}) {
  const server = config.servers[language.id] ?? {};
  const retryable = projects.filter((project) => project.state !== "Disabled");
  const failures = projects.filter((project) => typeof project.state === "object");
  // shortcut: use the detection message until the backend reports a structured missing-server status.
  const missingServer = retryable.length > 0 && retryable.every(({ state }) =>
    typeof state === "object" && state.Failed.startsWith("Code index executable not found: "));
  const [installRoot, setInstallRoot] = useState("");
  const installProject = failures.find((project) => project.root === installRoot) ?? failures[0];
  const canCancel = projects.length > 0 && (installation === "Installing" || installation === "Verifying");
  const canInstall = installProject && !canCancel && installation !== "Activating" && !server.command && server.enabled !== false;
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
    <fieldset disabled={disabled} style={cardStyle}>
      <legend style={srOnlyStyle}>{language.name}</legend>
      <div style={headingStyle}>
        <h4 style={titleStyle}>{language.name}</h4>
        <label style={toggleStyle}>
          <input type="checkbox" checked={!missingServer && server.enabled !== false} disabled={missingServer}
            style={{ margin: 0, accentColor: "var(--accent)" }}
            onChange={(event) => onChange(updateIndexServer(config, language.id, { enabled: event.target.checked }))} />
          Enable indexing
        </label>
      </div>
      {Array.from(new Set(failures.map((project) => stateText(project.state))), (message) =>
        <p style={errorStyle} key={message}>{message}</p>)}
      {installation && <p role="status" style={typeof installation === "object" ? errorStyle : hintStyle}>
        {typeof installation === "object" ? installation.Failed : {
          Installing: "Installing server…",
          Verifying: "Checking server version and initialization…",
          Activating: "Starting server in open workspaces…",
          Installed: "Server installation verified.",
          Cancelled: "Installation cancelled.",
        }[installation]}
      </p>}
      <details open={!!error || !!server.command || !!server.args?.length || undefined}>
        <summary style={summaryStyle}>Server configuration</summary>
        <div style={{ ...fieldStyle, marginTop: 12, marginBottom: 14 }}>
          <label htmlFor={`index-command-${language.id}`} style={labelStyle}>Server executable</label>
          <input id={`index-command-${language.id}`} placeholder={`${language.command} (PATH)`} value={server.command ?? ""}
            style={inputStyle}
            onChange={(event) => onChange(updateIndexServer(config, language.id, { command: event.target.value || null }))} />
          <p style={hintStyle}>Leave empty to use an Oaklet managed server or detect one from PATH.</p>
        </div>
        <div style={fieldStyle}>
          <label htmlFor={`index-args-${language.id}`} style={labelStyle}>Arguments</label>
          <input id={`index-args-${language.id}`} value={args} style={inputStyle} aria-invalid={!!error} aria-describedby={`index-args-hint-${language.id}`} onChange={(event) => editArgs(event.target.value)} />
          <p id={`index-args-hint-${language.id}`} style={error ? { ...errorStyle, gridColumn: 2 } : hintStyle}>
            {error || (language.id === "typescript" || language.id === "python"
              ? 'Custom arguments require an executable. Default: ["--stdio"].'
              : 'Custom arguments require an executable. Default: [].')}
          </p>
        </div>
      </details>
      {canInstall && language.id === "rust" && <>
        <p style={hintStyle}>Adds rust-analyzer to this project’s active Rust toolchain using rustup.</p>
        {failures.length > 1 && <div style={{ ...fieldStyle, marginTop: 8 }}>
          <label htmlFor="index-rust-install-project" style={labelStyle}>Project toolchain</label>
          <select id="index-rust-install-project" value={installProject.root} disabled={busy} style={{ ...inputStyle, fontFamily: "var(--sans)" }}
            onChange={(event) => setInstallRoot(event.target.value)}>
            {failures.map((project) => <option key={project.root} value={project.root}>{project.root}</option>)}
          </select>
        </div>}
      </>}
      {projects.length > 0 && <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "flex-end", gap: 8, marginTop: 8 }}>
        <button type="button" style={buttonStyle} disabled={busy || retryable.length === 0} onClick={() => onRetry(retryable)} title="Retry this language in the current workspace">Retry</button>
        {canInstall && <button className="primary" type="button" style={buttonStyle} disabled={busy} onClick={() => onInstall(installProject)}>
          {installation && installation !== "Installed" ? "Retry installation" : "Install server"}
        </button>}
        {canCancel && <button type="button" style={buttonStyle} disabled={busy} onClick={onCancelInstall}>Cancel installation</button>}
      </div>}
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
  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
    }
    catch (reason) { setError(errorText(reason)); }
    finally { setBusy(false); }
  }
  async function retry(projects: IndexProject[]) {
    const results = await Promise.allSettled(projects.map((project) =>
      invoke("code_index_retry", { projectRoot: project.root, language: project.language })));
    const failure = results.find((result) => result.status === "rejected");
    if (failure) throw failure.reason;
  }
  return (
    <div className="code-index-settings">
      <div className="index-heading"><h3>Current workspace</h3></div>
      {!status && !error && !streamError && <p className="field-hint" role="status">Loading index status…</p>}
      {(error || streamError) && <p className="error-text" role="alert">{error || streamError}</p>}
      {status && <WorkspaceIndexStatus status={status} disabled={disabled || busy} busy={busy}
        onRebuild={() => void act(() => invoke("code_index_rebuild"))} />}
      <section className="index-servers" aria-labelledby="index-servers-title">
        <div className="index-heading"><h3 id="index-servers-title">Language servers</h3><span className="field-hint">Global settings</span></div>
        <p className="field-hint">Save applies server settings to every open workspace.</p>
        <div className="index-server-list">
          {LANGUAGES.map((language) => <ServerFields key={language.id} language={language} config={config} disabled={disabled}
            busy={busy} projects={status?.projects.filter((project) => project.language === language.id) ?? []}
            installation={status?.installs?.[language.id]} onChange={onChange} onValidity={onValidity}
            onRetry={(projects) => void act(() => retry(projects))}
            onInstall={(project) => void act(() => invoke("code_index_install", { projectRoot: project.root, language: project.language }))}
            onCancelInstall={() => void act(() => invoke("code_index_cancel_install", { language: language.id }))} />)}
        </div>
        <details className="index-requirements">
          <summary>Required runtimes</summary>
          <p className="field-hint">Rust: an installed Rust toolchain and rustup for installation. TypeScript / JavaScript: Node.js 22.22.2+ and npm.</p>
          <p className="field-hint">Python: Node.js, npm and a Python interpreter. Go: Go 1.23.1 or newer. Java: JDK 21+ and Python 3 for the default JDT LS launcher. Project dependencies must already be available locally.</p>
        </details>
      </section>
    </div>
  );
}

export function WorkspaceIndexStatus({ status, disabled, busy, onRebuild }: {
  status: IndexStatus; disabled: boolean; busy: boolean;
  onRebuild: () => void;
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
      <div className="index-rebuild">
        <button type="button" disabled={disabled} onClick={onRebuild}>{busy ? "Working…" : "Clear and rebuild index"}</button>
      </div>
    </div>
  );
}
