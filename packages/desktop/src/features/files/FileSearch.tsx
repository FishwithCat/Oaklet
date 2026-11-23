import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Document2Regular } from "@mingcute/react/core-regular";
import { errorText } from "../../shared/error-text";
import { fileParts, matchFiles } from "./quick-open";

export function FileSearch({ workspace, onClose, onOpen }: {
  workspace: string;
  onClose: () => void;
  onOpen: (path: string) => void;
}) {
  const [files, setFiles] = useState<string[] | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const results = useMemo(() => matchFiles(files ?? [], query), [files, query]);

  useEffect(() => {
    let cancelled = false;
    invoke<string[]>("files_find", { workspace })
      .then((found) => { if (!cancelled) setFiles(found); })
      .catch((err) => { if (!cancelled) setError(errorText(err)); });
    return () => { cancelled = true; };
  }, [workspace]);

  useEffect(() => setActive(0), [query]);
  useEffect(() => {
    list.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active, results]);

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (results.length === 0) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((current) => (current + step + results.length) % results.length);
    } else if (event.key === "Enter" && !event.nativeEvent.isComposing) {
      event.preventDefault();
      const path = results[active];
      if (path) onOpen(path);
    } else if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  return (
    <div
      className="files-search-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="files-search" role="dialog" aria-modal="true" aria-label="Search files">
        <input
          className="files-search-input"
          autoFocus
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Search files by name"
          aria-label="Search files"
          role="combobox"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls="files-search-results"
          aria-activedescendant={results[active] ? `files-search-option-${active}` : undefined}
        />
        <div id="files-search-results" ref={list} className="files-search-list" role="listbox" aria-label="Matching files">
          {error ? <p className="files-search-status" role="alert">{error}</p>
            : files === null ? <p className="files-search-status">Loading files…</p>
              : results.length === 0 ? <p className="files-search-status">No files match.</p>
                : results.map((path, index) => {
                  const { name, dir } = fileParts(path);
                  return (
                    <button
                      key={path}
                      id={`files-search-option-${index}`}
                      type="button"
                      role="option"
                      aria-selected={index === active}
                      className="files-search-row"
                      title={path}
                      onMouseEnter={() => setActive(index)}
                      onClick={() => onOpen(path)}
                    >
                      <Document2Regular size={14} aria-hidden="true" />
                      <span className="files-search-name">{name}</span>
                      {dir && <span className="files-search-dir">{dir}</span>}
                    </button>
                  );
                })}
        </div>
      </div>
    </div>
  );
}
