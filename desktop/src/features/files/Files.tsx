import { Fragment, useEffect, useImperativeHandle, useMemo, useRef, useState, type Ref } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  CloseRegular,
  CodeRegular,
  Document2Regular,
  EyeRegular,
  FolderTreeRegular,
  Refresh3Regular,
  RightSmallRegular,
} from "@mingcute/react/core-regular";
import CodeMirror from "@uiw/react-codemirror";
import { EditorState, type Extension } from "@codemirror/state";
import { MarkdownText } from "../../shared/Markdown";
import { errorText } from "../../shared/error-text";
import { BranchMenu } from "../source-control/BranchMenu";
import { useGitBranch } from "../source-control/useGitBranch";
import { breadcrumbParts, editorText, savedText, type FileChange } from "./file-model";

import { FileTree } from "./FileTree";

export type FilesHandle = { canLeave: () => Promise<boolean> };
type Document = { path: string; original: string; content: string };

async function language(path: string): Promise<Extension> {
  const ext = path.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "js": case "jsx": case "ts": case "tsx": case "mjs": case "cjs":
      return (await import("@codemirror/lang-javascript")).javascript({ jsx: ext.endsWith("x"), typescript: ext === "ts" || ext === "tsx" });
    case "json": return (await import("@codemirror/lang-json")).json();
    case "md": case "mdx": return (await import("@codemirror/lang-markdown")).markdown();
    case "rs": return (await import("@codemirror/lang-rust")).rust();
    case "css": return (await import("@codemirror/lang-css")).css();
    case "html": case "htm": return (await import("@codemirror/lang-html")).html();
    default: return [];
  }
}

export default function Files({ ref, active, workspace, refreshToken, changes, onRefresh, onSaved }: {
  ref?: Ref<FilesHandle>;
  active: boolean;
  workspace: string;
  refreshToken: number;
  changes: readonly FileChange[] | null;
  onRefresh: () => void;
  onSaved: () => void;
}) {
  const [document, setDocument] = useState<Document | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState(false);
  const [syntax, setSyntax] = useState<Extension>([]);
  const busyRef = useRef(false);
  const dirty = document !== null && document.content !== editorText(document.original);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const leaveRef = useRef<() => Promise<boolean>>(async () => true);
  const extensions = useMemo(() => [syntax, EditorState.lineSeparator.of("\n")], [syntax]);
  const {
    branch,
    branches,
    branchLoaded,
    switchingBranch,
    branchError,
    switchBranch,
    clearBranchError,
  } = useGitBranch({ active, refreshToken, onRefresh });

  async function canLeave() {
    if (busyRef.current) return false;
    if (!dirtyRef.current) return true;
    busyRef.current = true;
    setBusy(true);
    try {
      return await confirm("Discard unsaved file changes?", { title: "Unsaved changes", kind: "warning", okLabel: "Discard", cancelLabel: "Keep editing" });
    } catch (err) {
      setError(errorText(err));
      return false;
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }
  leaveRef.current = canLeave;
  useImperativeHandle(ref, () => ({ canLeave }));

  useEffect(() => {
    const appWindow = getCurrentWindow();
    const unlisten = appWindow.onCloseRequested(async (event) => {
      if (!dirtyRef.current && !busyRef.current) return;
      event.preventDefault();
      if (await leaveRef.current()) {
        await appWindow.destroy().catch((err) => setError(errorText(err)));
      }
    });
    unlisten.catch((err) => setError(errorText(err)));
    function beforeUnload(event: BeforeUnloadEvent) {
      if (dirtyRef.current || busyRef.current) { event.preventDefault(); event.returnValue = ""; }
    }
    window.addEventListener("beforeunload", beforeUnload);
    return () => {
      void unlisten.then((stop) => stop()).catch(() => {});
      window.removeEventListener("beforeunload", beforeUnload);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setSyntax([]);
    if (document) void language(document.path)
      .then((value) => { if (!cancelled) setSyntax(value); })
      .catch(() => {}); // Plain text remains usable if a language chunk cannot load.
    return () => { cancelled = true; };
  }, [document?.path]);

  async function openFile(path: string) {
    if (!await canLeave()) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    try {
      const content = await invoke<string>("files_read", { workspace, path });
      setDocument({ path, original: content, content: editorText(content) });
      dirtyRef.current = false;
      setPreview(false);
    } catch (err) { setError(errorText(err)); }
    finally { busyRef.current = false; setBusy(false); }
  }

  async function save() {
    if (!document || !dirty || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    try {
      const content = savedText(document.content, document.original);
      await invoke("files_save", { workspace, path: document.path, original: document.original, content });
      setDocument({ ...document, original: content });
      dirtyRef.current = false;
      onSaved();
    } catch (err) { setError(errorText(err)); }
    finally { busyRef.current = false; setBusy(false); }
  }

  return (
    <section className="files-panel" hidden={!active} aria-label="Files" onKeyDown={(event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
    }}>
      <header className="view-bar">
        <span className="view-bar-icon" aria-hidden="true">
          <FolderTreeRegular size={14} />
        </span>
        <span className="view-bar-title">Files</span>
        {branchLoaded && (
          <BranchMenu
            active={active}
            branch={branch}
            branches={branches}
            switchingBranch={switchingBranch}
            branchError={branchError}
            onSwitch={switchBranch}
            onClearError={clearBranchError}
          />
        )}
        <button
          type="button"
          className="ghost icon-button view-bar-refresh"
          onClick={onRefresh}
          disabled={busy || switchingBranch !== null}
          aria-label="Refresh files"
          title="Refresh files"
        >
          <Refresh3Regular size={14} />
        </button>
      </header>
      <div className="files-body">
        <FileTree
          workspace={workspace}
          active={active}
          refreshToken={refreshToken}
          changes={changes}
          selected={document?.path}
          busy={busy}
          onOpen={(path) => { if (path !== document?.path) void openFile(path); }}
        />
        <div className="files-document">
          {error && <p role="alert" className="files-error">{error}</p>}
          {document ? <>
            <div className="files-toolbar">
              <nav className="files-breadcrumbs" aria-label="File path" title={document.path}>
                {breadcrumbParts(workspace, document.path).map((part, index, parts) => {
                  const last = index === parts.length - 1;
                  return (
                    <Fragment key={`${index}:${part}`}>
                      {index > 0 && <RightSmallRegular className="files-crumb-sep" size={10} aria-hidden="true" />}
                      <span className={`files-crumb${last ? " files-crumb-file" : ""}`} aria-current={last ? "page" : undefined}>
                        {last && <Document2Regular size={12} aria-hidden="true" />}
                        <span className="files-crumb-label">{part}</span>
                        {last && dirty && <><span className="files-dirty" aria-hidden="true">●</span><span className="sr-only">Unsaved changes</span></>}
                      </span>
                    </Fragment>
                  );
                })}
              </nav>
              <div className="files-actions">
                <button className="ghost icon-button" disabled={busy} aria-label="Reload file" title="Reload file" onClick={() => void openFile(document.path)}><Refresh3Regular size={14} /></button>
                {/\.(md|mdx)$/i.test(document.path) && <button className="ghost icon-button" aria-label="Preview" title={preview ? "Show source" : "Preview"} aria-pressed={preview} onClick={() => setPreview(!preview)}>{preview ? <CodeRegular size={14} /> : <EyeRegular size={14} />}</button>}
                <button className="ghost icon-button" disabled={busy} aria-label="Close file" title="Close file" onClick={async () => { if (await canLeave()) { setDocument(null); dirtyRef.current = false; } }}><CloseRegular size={14} /></button>
              </div>
            </div>
            {preview ? <div className="files-preview"><MarkdownText text={document.content} /></div> :
              <CodeMirror key={document.path} className="files-editor" value={document.content} height="100%" extensions={extensions}
                readOnly={busy} editable={!busy} aria-label={`File contents: ${document.path}`}
                onChange={(_, update) => {
                  const content = update.state.sliceDoc();
                  dirtyRef.current = content !== editorText(document.original);
                  setDocument({ ...document, content });
                }} />}
          </> : <div className="files-empty files-placeholder">Select a file to view or edit.</div>}
        </div>
      </div>
    </section>
  );
}
