import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { SearchRegular } from "@mingcute/react/core-regular";
import { filterCommits, relativeDate, type CommitInfo } from "./git-model";

export function CommitPicker({ commits, commit, onSelect }: {
  commits: readonly CommitInfo[];
  commit: CommitInfo;
  onSelect: (hash: string) => void;
}) {
  const id = useId();
  const listId = `${id}-commits`;
  const picker = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const results = filterCommits(commits, query);
  const activeIndex = Math.min(index, results.length - 1);
  const active = results[activeIndex];

  useEffect(() => {
    if (!open) return;
    function closeOutside(event: PointerEvent) {
      if (event.target instanceof Node && !picker.current?.contains(event.target)) setOpen(false);
    }
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [open]);

  useLayoutEffect(() => {
    list.current?.children[activeIndex]?.scrollIntoView({ block: "nearest" });
  }, [open, activeIndex, active?.hash]);

  function close() {
    setOpen(false);
    trigger.current?.focus({ preventScroll: true });
  }

  function choose(hash: string) {
    onSelect(hash);
    close();
  }

  function onSearchKey(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Enter" && active) {
      event.preventDefault();
      choose(active.hash);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!results.length) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setIndex(Math.max(0, Math.min(activeIndex + step, results.length - 1)));
    }
  }

  return (
    <div
      className="sc-commit-picker"
      ref={picker}
      onBlur={(event) => {
        if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
      onKeyDown={(event) => {
        if (!open || event.key !== "Escape" || event.nativeEvent.isComposing) return;
        event.preventDefault();
        event.stopPropagation();
        close();
      }}
    >
      <button
        type="button"
        ref={trigger}
        className="sc-control sc-commit-trigger"
        aria-label={`Search commits, selected ${commit.shortHash} ${commit.subject}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        title={`${commit.shortHash} ${commit.subject}`}
        onClick={() => {
          if (open) {
            close();
          } else {
            setQuery("");
            setIndex(Math.max(0, commits.findIndex((entry) => entry.hash === commit.hash)));
            setOpen(true);
          }
        }}
      >
        <span className="sc-commit-subject">{commit.subject || "Untitled commit"}</span>
        <SearchRegular size={14} aria-hidden="true" />
      </button>
      {open && (
        <div id={id} className="sc-commit-menu" role="dialog" aria-label="Search recent commits">
          <input
            autoFocus
            className="sc-control"
            type="search"
            value={query}
            spellCheck={false}
            onChange={(event) => { setQuery(event.target.value); setIndex(0); }}
            onKeyDown={onSearchKey}
            placeholder="Search recent commits"
            aria-label="Search recent commits"
            role="combobox"
            aria-autocomplete="list"
            aria-expanded="true"
            aria-controls={listId}
            aria-activedescendant={active ? `${listId}-${active.hash}` : undefined}
          />
          <div id={listId} ref={list} className="sc-commit-results" role="listbox" aria-label="Matching commits">
            {results.map((entry, row) => (
              <button
                type="button"
                role="option"
                key={entry.hash}
                id={`${listId}-${entry.hash}`}
                tabIndex={-1}
                aria-selected={row === activeIndex}
                className="sc-commit-option"
                title={entry.subject}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => choose(entry.hash)}
              >
                <span className="sc-commit-subject">{entry.subject || "Untitled commit"}</span>
                <span className="sc-commit-meta">
                  {entry.refs.length > 0 && (
                    <span className="sc-commit-refs">
                      {entry.refs.map((ref) => <span className="commit-ref" key={ref} title={ref}>{ref}</span>)}
                    </span>
                  )}
                  <code>{entry.shortHash}</code>
                  <span className="sc-commit-author">{entry.author}</span>
                  <time dateTime={entry.date}>{relativeDate(entry.date)}</time>
                </span>
              </button>
            ))}
          </div>
          {results.length === 0 && <p className="sc-commit-empty" role="status">No matching commits.</p>}
        </div>
      )}
    </div>
  );
}
