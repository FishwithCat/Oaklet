import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Document2Regular,
  DownSmallRegular,
  FolderOpenRegular,
  FolderRegular,
  RightSmallRegular,
} from "@mingcute/react/core-regular";
import { errorText } from "../../shared/error-text";
import { displayPath } from "../../shared/display-path";
import { statusInfo } from "../source-control/git-model";
import { changeMarkers, fileTreeRows, pathWithin, treeKey, workspaceName, type FileChange, type FileEntry } from "./file-model";

/** Show a load note only after a beat, so fast local reads never flash. */
function LoadingNote({ label }: { label: string }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setShown(true), 150);
    return () => window.clearTimeout(timer);
  }, []);
  return shown ? <p role="status" className="files-tree-status">{label}</p> : null;
}

export function FileTree({ workspace, active, refreshToken, changes, selected, busy, onOpen, onCopyPath, onMutate, onRefresh, onError }: {
  workspace: string;
  active: boolean;
  refreshToken: number;
  changes: readonly FileChange[] | null;
  selected?: string;
  busy: boolean;
  onOpen: (path: string) => void;
  onCopyPath: (path: string) => void;
  onMutate: (entry: FileEntry, name?: string) => Promise<{ path: string | null } | undefined>;
  onRefresh: () => void;
  onError: (error: string) => void;
}) {
  const [directories, setDirectories] = useState<Record<string, FileEntry[]>>({});
  const [expanded, setExpanded] = useState(new Set(["."]));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(new Set<string>());
  const [focused, setFocused] = useState<string>();
  const [menu, setMenu] = useState<{ entry: FileEntry; x: number; y: number }>();
  const [renameEntry, setRenameEntry] = useState<FileEntry>();
  const [renameError, setRenameError] = useState("");
  const menuRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const renameTitle = useId();
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

  useLayoutEffect(() => {
    const element = menuRef.current;
    if (!menu || !element) return;
    element.style.left = `${Math.max(4, Math.min(menu.x, window.innerWidth - element.offsetWidth - 4))}px`;
    element.style.top = `${Math.max(4, Math.min(menu.y, window.innerHeight - element.offsetHeight - 4))}px`;
    element.querySelector("button")?.focus({ preventScroll: true });
  }, [menu]);

  useEffect(() => {
    if (!menu) return;
    function dismissOutside(event: PointerEvent) {
      if (!menuRef.current?.contains(event.target as Node)) setMenu(undefined);
    }
    function dismiss() { setMenu(undefined); }
    document.addEventListener("pointerdown", dismissOutside);
    window.addEventListener("blur", dismiss);
    window.addEventListener("resize", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismissOutside);
      window.removeEventListener("blur", dismiss);
      window.removeEventListener("resize", dismiss);
    };
  }, [menu]);

  useEffect(() => {
    if (active) return;
    setMenu(undefined);
    setRenameEntry(undefined);
  }, [active]);

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!active || !renameEntry || !dialog) return;
    dialog.showModal();
    dialog.querySelector("input")?.select();
    return () => dialog.close();
  }, [active, renameEntry]);

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

  function showMenu(entry: FileEntry, x: number, y: number) {
    setFocused(entry.path);
    setMenu({ entry, x, y });
  }

  function closeMenu(restoreFocus = false) {
    if (restoreFocus && menu) focus(menu.entry.path);
    setMenu(undefined);
  }

  function navigateMenu(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu(true);
      return;
    }
    if (event.key === "Tab") {
      closeMenu(true);
      return;
    }
    const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
    const index = items.indexOf(event.target as HTMLButtonElement);
    const last = items.length - 1;
    let next: number;
    switch (event.key) {
      case "ArrowDown": next = index === last ? 0 : index + 1; break;
      case "ArrowUp": next = index <= 0 ? last : index - 1; break;
      case "Home": next = 0; break;
      case "End": next = last; break;
      default: return;
    }
    event.preventDefault();
    items[next]?.focus();
  }

  function applyMutation(entry: FileEntry, path: string | null) {
    if (path === entry.path) return;
    const separator = entry.path.lastIndexOf("/");
    const parent = separator < 0 ? "." : entry.path.slice(0, separator);
    focus(parent);
    setExpanded((current) => {
      const next = new Set(current);
      for (const expandedPath of current) {
        if (!pathWithin(expandedPath, entry.path)) continue;
        next.delete(expandedPath);
        if (path !== null) next.add(path + expandedPath.slice(entry.path.length));
      }
      return next;
    });
    setDirectories((current) => {
      const next = { ...current };
      for (const directory of Object.keys(next)) {
        if (pathWithin(directory, entry.path)) delete next[directory];
      }
      next[parent] = current[parent]?.filter((item) => item.path !== entry.path) ?? [];
      return next;
    });
    onRefresh();
  }

  async function deleteEntry(entry: FileEntry) {
    closeMenu(true);
    try {
      const result = await onMutate(entry);
      if (result) applyMutation(entry, result.path);
    } catch (error) {
      onError(errorText(error));
      onRefresh();
    }
  }

  async function submitRename(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!renameEntry || busy) return;
    const name = String(new FormData(event.currentTarget).get("name") ?? "");
    setRenameError("");
    try {
      const result = await onMutate(renameEntry, name);
      if (!result) return;
      dialogRef.current?.close();
      setRenameEntry(undefined);
      applyMutation(renameEntry, result.path);
    } catch (error) {
      setRenameError(errorText(error));
      dialogRef.current?.querySelector("input")?.focus();
    }
  }

  function navigate(event: KeyboardEvent, index: number) {
    const { entry, parent } = rows[index];
    switch (event.key) {
      case "ContextMenu":
      case "F10": {
        if (event.key === "F10" && !event.shiftKey) return;
        const row = event.currentTarget.getBoundingClientRect();
        showMenu(entry, row.left + 16, row.bottom);
        break;
      }
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
    <nav className="files-browser" aria-label="Workspace files" onScroll={() => closeMenu()}>
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
                aria-haspopup="menu"
                tabIndex={tabPath === entry.path ? 0 : -1}
                className={`ghost files-entry${entry.path === "." ? " files-root" : ""}${selected === entry.path ? " selected" : ""}`}
                style={{ paddingLeft: 8 + depth * 16 }}
                title={entry.path === "." ? displayPath(workspace) : entry.path}
                onFocus={() => setFocused(entry.path)}
                onKeyDown={(event) => navigate(event, index)}
                onContextMenu={(event) => {
                  event.preventDefault();
                  showMenu(entry, event.clientX, event.clientY);
                }}
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
      {active && menu && (
        <div ref={menuRef} className="files-context-menu" role="menu" aria-label={`Actions for ${menu.entry.name}`}
          onKeyDown={navigateMenu} onContextMenu={(event) => event.preventDefault()}>
          <button type="button" role="menuitem" className="ghost" tabIndex={-1} onClick={() => {
            closeMenu(true);
            onCopyPath(menu.entry.path);
          }}>Copy Path</button>
          {menu.entry.path !== "." && <>
            <button type="button" role="menuitem" className="ghost" tabIndex={-1} disabled={busy} onClick={() => {
              closeMenu(true);
              setRenameError("");
              setRenameEntry(menu.entry);
            }}>Rename</button>
            <button type="button" role="menuitem" className="ghost files-context-delete" tabIndex={-1} disabled={busy}
              onClick={() => void deleteEntry(menu.entry)}>Delete</button>
          </>}
        </div>
      )}
      {active && renameEntry && (
        <dialog ref={dialogRef} className="modal files-rename" aria-labelledby={renameTitle}
          onKeyDown={(event) => event.stopPropagation()} onCancel={(event) => {
          event.preventDefault();
          if (!busy) setRenameEntry(undefined);
        }}>
          <form onSubmit={submitRename}>
            <h2 id={renameTitle}>{renameEntry.directory ? "Rename folder" : "Rename file"}</h2>
            <label>
              New name
              <input name="name" defaultValue={renameEntry.name} required readOnly={busy} />
            </label>
            {renameError && <p role="alert" className="files-error">{renameError}</p>}
            <div className="modal-actions">
              <button type="button" className="ghost" disabled={busy} onClick={() => setRenameEntry(undefined)}>Cancel</button>
              <button type="submit" className="primary" disabled={busy}>Rename</button>
            </div>
          </form>
        </dialog>
      )}
    </nav>
  );
}
