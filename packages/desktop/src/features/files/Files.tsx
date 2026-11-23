import { Fragment, lazy, Suspense, useEffect, useImperativeHandle, useRef, useState, type CSSProperties, type Ref } from "react";
import { invoke } from "@tauri-apps/api/core";
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
import type { Extension } from "@codemirror/state";
import { ImageLightbox } from "../../shared/ImageLightbox";
import { MarkdownText } from "../../shared/Markdown";
import { PaneResizer } from "../../shared/PaneResizer";
import { errorText } from "../../shared/error-text";
import { previewFontShortcut } from "../../app/shortcuts";
import { changedLines, isImagePath, parseDiff, type ChangedLine } from "../source-control/git-diff";
import { breadcrumbParts, editorText, isMarkdownPath, savedText, type FileChange } from "./file-model";

import { FileTree } from "./FileTree";

// The editor bundle is heavy; load it when a file is opened, not with the page.
const FileEditor = lazy(() => import("./FileEditor"));

const PREVIEW_FONT_SIZE_DEFAULT = 12;
const PREVIEW_FONT_SIZE_MIN = 10;
const PREVIEW_FONT_SIZE_MAX = 20;
const NO_CHANGES: readonly ChangedLine[] = [];

export type FilesHandle = {
  canLeave: () => Promise<boolean>;
  hasUnsaved: () => boolean;
  open: (path: string) => void;
  closeDocument: () => void;
};
type Document = { path: string; original: string; content: string };
type ImageDocument = { path: string; src: string };

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
  const [image, setImage] = useState<ImageDocument | null>(null);
  const [enlarged, setEnlarged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState(false);
  const [previewFontSize, setPreviewFontSize] = useState(PREVIEW_FONT_SIZE_DEFAULT);
  const [syntax, setSyntax] = useState<Extension>([]);
  const [lineMarks, setLineMarks] = useState<{ path: string; marks: readonly ChangedLine[] } | null>(null);
  const [browserWidth, setBrowserWidth] = useState(300);
  const busyRef = useRef(false);
  const dirty = document !== null && document.content !== editorText(document.original);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;

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

  useImperativeHandle(ref, () => ({
    canLeave,
    hasUnsaved: () => dirtyRef.current || busyRef.current,
    open: (path) => { void openFile(path, isMarkdownPath(path)); },
    closeDocument: () => {
      setDocument(null);
      setImage(null);
      setEnlarged(false);
      setPreview(false);
      dirtyRef.current = false;
    },
  }));

  useEffect(() => {
    let cancelled = false;
    setSyntax([]);
    if (document) void language(document.path)
      .then((value) => { if (!cancelled) setSyntax(value); })
      .catch(() => {}); // Plain text remains usable if a language chunk cannot load.
    return () => { cancelled = true; };
  }, [document?.path]);

  useEffect(() => {
    if (!active || !preview) return;
    function onKeyDown(event: KeyboardEvent) {
      const action = previewFontShortcut(event);
      if (action === 0) return;
      event.preventDefault();
      setPreviewFontSize((size) =>
        action === "reset"
          ? PREVIEW_FONT_SIZE_DEFAULT
          : Math.min(PREVIEW_FONT_SIZE_MAX, Math.max(PREVIEW_FONT_SIZE_MIN, size + action)),
      );
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active, preview]);

  // Gutter marks come from the file on disk. While the buffer is dirty it
  // keeps its mapped marks, and saving refetches the diff.
  useEffect(() => {
    if (!document || dirty) return;
    let cancelled = false;
    const path = document.path;
    invoke<string>("git_diff", { path })
      .then((patch) => {
        if (!cancelled) setLineMarks({ path, marks: changedLines(parseDiff(patch)) });
      })
      .catch(() => {
        if (!cancelled) setLineMarks({ path, marks: NO_CHANGES });
      });
    return () => { cancelled = true; };
  }, [document?.path, dirty, changes]);

  async function openFile(path: string, previewMarkdown = false) {
    if (!await canLeave()) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    try {
      if (isImagePath(path)) {
        const src = await invoke<string>("files_read_image", { workspace, path });
        setImage({ path, src });
        setEnlarged(false);
        setDocument(null);
        setPreview(false);
        dirtyRef.current = false;
      } else {
        const content = await invoke<string>("files_read", { workspace, path });
        setDocument({ path, original: content, content: editorText(content) });
        setImage(null);
        dirtyRef.current = false;
        setPreview(previewMarkdown);
      }
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

  const openPath = document?.path ?? image?.path;

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
        <button
          type="button"
          className="ghost icon-button view-bar-refresh"
          onClick={onRefresh}
          disabled={busy}
          aria-label="Refresh files"
          title="Refresh files"
        >
          <Refresh3Regular size={14} />
        </button>
      </header>
      <div className="files-body" style={{ "--files-browser-width": `${browserWidth}px` } as CSSProperties}>
        <FileTree
          workspace={workspace}
          active={active}
          refreshToken={refreshToken}
          changes={changes}
          selected={openPath}
          busy={busy}
          onOpen={(path) => { if (path !== openPath) void openFile(path); }}
        />
        <PaneResizer width={browserWidth} label="Resize file browser" onChange={setBrowserWidth} />
        <div className="files-document">
          {error && <p role="alert" className="files-error">{error}</p>}
          {openPath ? <>
            <div className="files-toolbar">
              <nav className="files-breadcrumbs" aria-label="File path" title={openPath}>
                {breadcrumbParts(workspace, openPath).map((part, index, parts) => {
                  const last = index === parts.length - 1;
                  return (
                    <Fragment key={`${index}:${part}`}>
                      {index > 0 && <RightSmallRegular className="files-crumb-sep" size={10} aria-hidden="true" />}
                      <span className={`files-crumb${last ? " files-crumb-file" : ""}`} aria-current={last ? "page" : undefined}>
                        {last && <Document2Regular size={12} aria-hidden="true" />}
                        <span className="files-crumb-label">{part}</span>
                        {last && document && dirty && <><span className="files-dirty" aria-hidden="true">●</span><span className="sr-only">Unsaved changes</span></>}
                      </span>
                    </Fragment>
                  );
                })}
              </nav>
              <div className="files-actions">
                <button className="ghost icon-button" disabled={busy} aria-label="Reload file" title="Reload file" onClick={() => void openFile(openPath)}><Refresh3Regular size={14} /></button>
                {document && isMarkdownPath(document.path) && <button className="ghost icon-button" aria-label="Preview" title={preview ? "Show source" : "Preview"} aria-pressed={preview} onClick={() => setPreview(!preview)}>{preview ? <CodeRegular size={14} /> : <EyeRegular size={14} />}</button>}
                <button className="ghost icon-button" disabled={busy} aria-label="Close file" title="Close file" onClick={async () => { if (await canLeave()) { setDocument(null); setImage(null); dirtyRef.current = false; } }}><CloseRegular size={14} /></button>
              </div>
            </div>
            {image ? <div className="files-image"><button type="button" className="files-image-button" title="Enlarge image" aria-label={`Enlarge ${image.path}`} onClick={() => setEnlarged(true)}><img src={image.src} alt={image.path} /></button></div> :
              document && (preview ? <div className="files-preview" style={{ "--files-preview-font-size": `${previewFontSize}px` } as CSSProperties}><MarkdownText text={document.content} /></div> :
                <Suspense fallback={<div className="files-editor" />}>
                  <FileEditor path={document.path} value={document.content} syntax={syntax} readOnly={busy}
                    marks={lineMarks?.path === document.path ? lineMarks.marks : NO_CHANGES}
                    onChange={(content) => {
                      dirtyRef.current = content !== editorText(document.original);
                      setDocument({ ...document, content });
                    }} />
                </Suspense>)}
          </> : <div className="files-empty files-placeholder">Select a file to view or edit.</div>}
        </div>
      </div>
      {image && enlarged && <ImageLightbox src={image.src} alt={image.path} onClose={() => setEnlarged(false)} />}
    </section>
  );
}
