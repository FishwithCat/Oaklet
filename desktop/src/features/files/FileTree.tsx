import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Document2Regular,
  DownSmallRegular,
  FolderOpenRegular,
  FolderRegular,
  RightSmallRegular,
} from "@mingcute/react/core-regular";
import { errorText } from "../../shared/error-text";
import { statusInfo } from "../source-control/git-model";
import { changeMarkers, fileTreeRows, treeKey, workspaceName, type FileChange, type FileEntry } from "./file-model";

/** Show a load note only after a beat, so fast local reads never flash. */
function LoadingNote({ label }: { label: string }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setShown(true), 150);
    return () => window.clearTimeout(timer);
  }, []);
  return shown ? <p role="status" className="files-tree-status">{label}</p> : null;
}

export function FileTree({ workspace, active, refreshToken, changes, selected, busy, onOpen }: {
  workspace: string;
  active: boolean;
  refreshToken: number;
  changes: readonly FileChange[] | null;
  selected?: string;
  busy: boolean;
  onOpen: (path: string) => void;
}) {
  const [directories, setDirectories] = useState<Record<string, FileEntry[]>>({});
  const [expanded, setExpanded] = useState(new Set(["."]));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(new Set<string>());
  const [focused, setFocused] = useState<string>();
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  const generation = useRef(0);
  const buttons = useRef(new Map<string, HTMLButtonElement>());
  const root = { name: workspaceName(workspace), path: ".", directory: true };
  const markers = useMemo(() => changeMarkers(changes ?? []), [changes]);
  const rows = [
    { entry: root, depth: 0, parent: ".", position: 1, siblings: 1 },
    ...fileTreeRows(directories, expanded).map((row) => ({ ...row, depth: row.depth + 1 })),
  ];
  const tabPath = rows.some(({ entry }) => entry.path === focused) ? focused : rows[0]?.entry.path;

  async function load(path: string, request: number) {
    setLoading((current) => new Set(current).add(path));
    setErrors((current) => ({ ...current, [path]: "" }));
    try {
      const entries = await invoke<FileEntry[]>("files_list", { workspace, path });
      if (generation.current === request) {
        setDirectories((current) => ({ ...current, [path]: entries }));
      }
    } catch (error) {
      if (generation.current === request) {
        setDirectories((current) => ({ ...current, [path]: [] }));
        setErrors((current) => ({ ...current, [path]: errorText(error) }));
      }
    } finally {
      if (generation.current === request) {
        setLoading((current) => {
          const next = new Set(current);
          next.delete(path);
          return next;
        });
      }
    }
  }

  useEffect(() => {
    if (!active) return;
    const request = ++generation.current;
    setLoading(new Set());
    for (const path of new Set([".", ...expandedRef.current])) void load(path, request);
    return () => { generation.current++; };
  }, [workspace, active, refreshToken]);

  function toggle(path: string) {
    const next = new Set(expanded);
    if (next.has(path)) next.delete(path);
    else {
      next.add(path);
      if ((!directories[path] || errors[path]) && !loading.has(path)) {
        void load(path, generation.current);
      }
    }
    setExpanded(next);
  }

  function focus(path?: string) {
    if (!path) return;
    setFocused(path);
    buttons.current.get(path)?.focus();
  }

  function navigate(event: KeyboardEvent, index: number) {
    const { entry, parent } = rows[index];
    switch (event.key) {
      case "ArrowDown": focus(rows[index + 1]?.entry.path); break;
      case "ArrowUp": focus(rows[index - 1]?.entry.path); break;
      case "Home": focus(rows[0]?.entry.path); break;
      case "End": focus(rows[rows.length - 1]?.entry.path); break;
      case "ArrowRight":
        if (entry.directory) {
          if (!expanded.has(entry.path)) toggle(entry.path);
          else if (rows[index + 1]?.parent === entry.path) focus(rows[index + 1].entry.path);
        }
        break;
      case "ArrowLeft":
        if (entry.directory && expanded.has(entry.path)) toggle(entry.path);
        else if (parent !== entry.path) focus(parent);
        break;
      default: return;
    }
    event.preventDefault();
  }

  return (
    <nav className="files-browser" aria-label="Workspace files">
      {errors["."] && <p role="alert" className="files-error">{errors["."]}</p>}
      {loading.has(".") && <LoadingNote label="Loading files…" />}
      <div role="tree" aria-label="Workspace files">
        {rows.map(({ entry, depth, position, siblings }, index) => {
          const open = entry.directory && expanded.has(entry.path);
          const key = treeKey(entry.path);
          const info = entry.directory ? undefined : markers.files.get(key);
          const status = info ? statusInfo(info.status) : null;
          const tone = info ? ` files-entry-${info.status}` : "";
          const folderChanged = entry.directory && key !== "." && markers.folders.has(key);
          return (
            <div key={entry.path} role="none">
              <button
                ref={(button) => { if (button) buttons.current.set(entry.path, button); else buttons.current.delete(entry.path); }}
                type="button"
                role="treeitem"
                aria-level={depth + 1}
                aria-posinset={position}
                aria-setsize={siblings}
                aria-expanded={entry.directory ? open : undefined}
                aria-selected={selected === entry.path}
                aria-disabled={!entry.directory && busy}
                tabIndex={tabPath === entry.path ? 0 : -1}
                className={`ghost files-entry${entry.path === "." ? " files-root" : ""}${selected === entry.path ? " selected" : ""}`}
                style={{ paddingLeft: 8 + depth * 16 }}
                title={entry.path === "." ? workspace : entry.path}
                onFocus={() => setFocused(entry.path)}
                onKeyDown={(event) => navigate(event, index)}
                onClick={() => entry.directory ? toggle(entry.path) : !busy && onOpen(entry.path)}
              >
                {Array.from({ length: Math.max(0, depth - 1) }, (_, level) => (
                  <span key={level} className="files-tree-guide" aria-hidden="true" style={{ left: 30 + level * 16 }} />
                ))}
                {entry.directory ? (open ? <DownSmallRegular size={12} /> : <RightSmallRegular size={12} />) : <span className="files-tree-spacer" />}
                {entry.directory ? (open ? <FolderOpenRegular size={16} /> : <FolderRegular size={16} />) : <Document2Regular size={16} />}
                <span className={`files-entry-label${tone}`}>{entry.name}</span>
                {info && status && (
                  <span className={`files-entry-marker${tone}`} title={status.title}>
                    {status.label}
                  </span>
                )}
                {folderChanged && <span className="files-entry-dot" title="Contains changes" aria-hidden="true" />}
              </button>
              {open && entry.path !== "." && loading.has(entry.path) && <LoadingNote label={`Loading ${entry.name}…`} />}
              {open && entry.path !== "." && errors[entry.path] && <p role="alert" className="files-error">{errors[entry.path]}</p>}
              {open && !loading.has(entry.path) && !errors[entry.path] && directories[entry.path]?.length === 0 && <p className="files-tree-status" style={{ paddingLeft: 40 + depth * 16 }}>Empty folder</p>}
            </div>
          );
        })}
      </div>
    </nav>
  );
}
