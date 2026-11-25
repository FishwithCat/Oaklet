import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorText } from "../../shared/error-text";
import { HistoryRegular } from "@mingcute/react/core-regular";
import type { SessionSummary } from "./session-model";

export function SessionHistory({
  sessions,
  onPick,
  onClose,
  onArchive,
}: {
  sessions: SessionSummary[];
  onPick: (id: string) => void;
  onClose: () => void;
  onArchive: (id: string, archived: boolean) => Promise<string | null>;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);
  const [archived, setArchived] = useState(false);
  const [archiveRows, setArchiveRows] = useState<SessionSummary[]>([]);
  const [archiveStatus, setArchiveStatus] = useState("");
  const [updating, setUpdating] = useState(false);
  const entries = archived ? archiveRows : sessions;
  const archiveRequest = useRef(0);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ message: string }>("worktree://archive", ({ payload }) => {
      if (!disposed) setArchiveStatus(payload.message);
    }).then((release) => {
      if (disposed) release();
      else unlisten = release;
    }).catch(console.error);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  async function refreshArchived() {
    const sequence = ++archiveRequest.current;
    try {
      const next = await invoke<SessionSummary[]>("session_list", { archived: true });
      if (sequence === archiveRequest.current) setArchiveRows(next);
    } catch (error) {
      if (sequence === archiveRequest.current) setArchiveStatus(errorText(error));
    }
  }

  async function archive(session: SessionSummary) {
    setUpdating(true);
    setArchiveStatus(archived ? "Restoring chat…" : "Archiving chat…");
    try {
      const result = await onArchive(session.id, !archived);
      if (archived) await refreshArchived();
      setArchiveStatus(result ?? (archived ? "Chat restored." : "Chat archived."));
    } catch (error) {
      setArchiveStatus(errorText(error));
    } finally {
      setUpdating(false);
    }
  }

  const needle = query.trim().toLowerCase();
  const matches = needle
    ? entries.filter((session) => session.title.toLowerCase().includes(needle))
    : entries;
  const selected = Math.min(active, matches.length - 1);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  useEffect(() => {
    if (selected >= 0) {
      listRef.current?.children[selected]?.scrollIntoView({ block: "nearest" });
    }
  }, [selected]);

  function onInputKeyDown(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" && matches.length > 0) {
      event.preventDefault();
      setActive(Math.min(selected + 1, matches.length - 1));
    } else if (event.key === "ArrowUp" && matches.length > 0) {
      event.preventDefault();
      setActive(Math.max(selected - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const session = matches[selected];
      if (session && !archived) onPick(session.id);
    }
  }

  return (
    <div
      className="modal-backdrop palette-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="session-palette"
        role="dialog"
        aria-modal="true"
        aria-label="Session history"
      >
        <input
          className="session-search"
          type="text"
          value={query}
          placeholder="Search sessions by title..."
          autoFocus
          spellCheck={false}
          role="combobox"
          aria-expanded={matches.length > 0}
          aria-controls="session-results"
          aria-autocomplete="list"
          aria-activedescendant={
            matches[selected] ? `session-${matches[selected].id}` : undefined
          }
          onChange={(event) => {
            setQuery(event.target.value);
            setActive(0);
          }}
          onKeyDown={onInputKeyDown}
        />

        {matches.length === 0 ? (
          <p className="session-empty">
            {entries.length === 0
              ? (archived ? "No archived sessions." : "No saved sessions yet.")
              : "No matching sessions."}
          </p>
        ) : (
          <ul
            className="session-results"
            id="session-results"
            role="listbox"
            ref={listRef}
          >
            {matches.map((session, index) => {
              const turns = session.turns;
              return (
                <li
                  key={session.id}
                  id={`session-${session.id}`}
                  role="option"
                  aria-selected={index === selected}
                  className={`session-row${index === selected ? " active" : ""}`}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => archived ? setActive(index) : onPick(session.id)}
                >
                  <HistoryRegular
                    className="session-row-icon"
                    size={14}
                  />
                  <span className="session-row-title">{session.title}</span>
                  <span className="session-row-meta">
                    {turns} {turns === 1 ? "turn" : "turns"} ·{" "}
                    {new Date(session.updatedAt).toLocaleString()}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
        <div className="session-history-actions">
          <button type="button" aria-pressed={archived} disabled={updating} onClick={() => {
            if (!archived) void refreshArchived();
            setArchived(!archived);
            setActive(0);
            setArchiveStatus("");
          }}>Archived</button>
          {selected >= 0 && (
            <button type="button" disabled={updating || matches[selected].running || matches[selected].compacting}
              onClick={() => void archive(matches[selected])}>
              {archived ? "Unarchive selected" : "Archive selected"}
            </button>
          )}
          {archiveStatus && <span role="status">{archiveStatus}</span>}
        </div>
      </div>
    </div>
  );
}
