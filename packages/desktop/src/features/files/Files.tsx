import { Fragment, lazy, Suspense, useEffect, useImperativeHandle, useRef, useState, type CSSProperties, type Ref } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  CheckRegular,
  CloseRegular,
  CodeRegular,
  CopyRegular,
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
import { supportsOutline } from "../../shared/source-symbol";
import { FileOutline } from "./FileOutline";
import type { FileEditorHandle } from "./FileEditor";
import { fileLanguage } from "./file-language";
import { previewFontShortcut } from "../../app/shortcuts";
import { changedLines, isImagePath, parseDiff, type ChangedLine } from "../source-control/git-diff";
import { absolutePath, breadcrumbParts, editorText, isMarkdownPath, pathWithin, savedText, type FileChange, type FileEntry } from "./file-model";

import { FileTree } from "./FileTree";
import { FileSearch } from "./FileSearch";

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
  quickOpen: () => void;
};
type Document = { path: string; original: string; content: string };
type ImageDocument = { path: string; src: string };

export default function Files({ ref, active, workspace, refreshToken, changes, onRefresh, onSaved, onQuote }: {
  ref?: Ref<FilesHandle>;
  active: boolean;
  workspace: string;
  refreshToken: number;
  changes: readonly FileChange[] | null;
  onRefresh: () => void;
  onSaved: () => void;
  onQuote?: (quote: string) => void;
}) {
  const [document, setDocument] = useState<Document | null>(null);
  const [image, setImage] = useState<ImageDocument | null>(null);
  const [enlarged, setEnlarged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [preview, setPreview] = useState(false);
  const [previewFontSize, setPreviewFontSize] = useState(PREVIEW_FONT_SIZE_DEFAULT);
  const [syntax, setSyntax] = useState<Extension>([]);
  const [editorReadyPath, setEditorReadyPath] = useState<string | null>(null);
  const [lineMarks, setLineMarks] = useState<{ path: string; marks: readonly ChangedLine[] } | null>(null);
  const [browserWidth, setBrowserWidth] = useState(300);
  const [copiedPath, setCopiedPath] = useState<string | null>(null);
  const busyRef = useRef(false);
  const editor = useRef<FileEditorHandle>(null);
  const dirty = document !== null && document.content !== editorText(document.original);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const openPath = document?.path ?? image?.path;

  function closeDocument() {
    setDocument(null);
    setImage(null);
    setEnlarged(false);
    setPreview(false);
    dirtyRef.current = false;
  }

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
    closeDocument,
    quickOpen: () => setSearchOpen(true),
  }));

  useEffect(() => {
    let cancelled = false;
    setSyntax([]);
    if (document) void fileLanguage(document.path)
      .then((value) => { if (!cancelled && value) setSyntax(value); })
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

  useEffect(() => {
    if (!active) setSearchOpen(false);
  }, [active]);

  useEffect(() => {
    if (copiedPath === null) return;
    const timeout = window.setTimeout(() => setCopiedPath(null), 1200);
    return () => window.clearTimeout(timeout);
  }, [copiedPath]);

  // Gutter marks come from the file on disk. While the buffer is dirty it
  // keeps its mapped marks, and saving refetches the diff.
  useEffect(() => {
    if (!document || dirty) return;
    let cancelled = false;
    const path = document.path;
    invoke<string>("git_diff", { path, workspace })
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

  async function copyPath(path: string) {
    try {
      await navigator.clipboard.writeText(absolutePath(workspace, path));
      setCopiedPath(path);
    } catch {
      setError("Could not copy the path.");
    }
  }

  async function mutateEntry(entry: FileEntry, name?: string) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    const { path } = entry;
    const affected = openPath && pathWithin(openPath, path) ? openPath : undefined;
    try {
      if (name === undefined) {
        const target = entry.directory ? `the folder “${path}” and all its contents` : `“${path}”`;
        const unsaved = affected && dirtyRef.current ? " Unsaved changes to the open file will be lost." : "";
        if (!await confirm(`Permanently delete ${target}?${unsaved}`, {
          title: entry.directory ? "Delete folder" : "Delete file",
          kind: "warning",
          okLabel: "Delete",
          cancelLabel: "Cancel",
        })) return;
        await invoke("files_delete", { workspace, path });
        if (affected) closeDocument();
        return { path: null };
      }

      const renamed = await invoke<string>("files_rename", { workspace, path, name });
      if (affected && renamed !== path) {
        const next = renamed + affected.slice(path.length);
        setDocument((current) => current && { ...current, path: next });
        setImage((current) => current && { ...current, path: next });
        if (!isMarkdownPath(next)) setPreview(false);
      }
      return { path: renamed };
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
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
          onCopyPath={copyPath}
          onMutate={mutateEntry}
          onRefresh={onRefresh}
          onError={setError}
        />
        <span className="sr-only" role="status">{copiedPath !== null ? "Path copied." : ""}</span>
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
              <button
                type="button"
                className="ghost icon-button"
                aria-label="Copy file path"
                title={copiedPath === openPath ? "File path copied" : "Copy file path"}
                onClick={() => void copyPath(openPath)}
              >
                {copiedPath === openPath
                  ? <CheckRegular size={14} aria-hidden="true" />
                  : <CopyRegular size={14} aria-hidden="true" />}
              </button>
              <div className="files-actions">
                {document && supportsOutline(document.path) && (
                  <FileOutline key={document.path} workspace={workspace} path={document.path}
                    source={document.original} dirty={dirty} busy={busy || editorReadyPath !== document.path}
                    onJump={(line) => editor.current?.jumpToLine(line)} />
                )}
                <button className="ghost icon-button" disabled={busy} aria-label="Reload file" title="Reload file" onClick={() => void openFile(openPath)}><Refresh3Regular size={14} /></button>
                {document && isMarkdownPath(document.path) && <button className="ghost icon-button" aria-label="Preview" title={preview ? "Show source" : "Preview"} aria-pressed={preview} onClick={() => setPreview(!preview)}>{preview ? <CodeRegular size={14} /> : <EyeRegular size={14} />}</button>}
                <button className="ghost icon-button" disabled={busy} aria-label="Close file" title="Close file" onClick={async () => { if (await canLeave()) closeDocument(); }}><CloseRegular size={14} /></button>
              </div>
            </div>
            {image ? <div className="files-image"><button type="button" className="files-image-button" title="Enlarge image" aria-label={`Enlarge ${image.path}`} onClick={() => setEnlarged(true)}><img src={image.src} alt={image.path} /></button></div> :
              document && (preview ? <div className="files-preview" style={{ "--files-preview-font-size": `${previewFontSize}px` } as CSSProperties}><MarkdownText text={document.content} /></div> :
                <Suspense fallback={<div className="files-editor" />}>
                  <FileEditor key={document.path} ref={editor} path={document.path} value={document.content} syntax={syntax} readOnly={busy}
                    onReadyChange={setEditorReadyPath}
                    onQuote={active ? onQuote : undefined}
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
      {searchOpen && (
        <FileSearch
          workspace={workspace}
          onClose={() => setSearchOpen(false)}
          onOpen={(path) => {
            setSearchOpen(false);
            if (path !== openPath) void openFile(path);
          }}
        />
      )}
    </section>
  );
}
