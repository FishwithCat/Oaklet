import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { ClockCounterClockwiseIcon } from "@phosphor-icons/react";
import type { SessionSnapshot } from "./session-history";

export function SessionHistory({
  sessions,
  onPick,
  onClose,
}: {
  sessions: SessionSnapshot[];
  onPick: (id: string) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);

  const needle = query.trim().toLowerCase();
  const matches = needle
    ? sessions.filter((session) => session.title.toLowerCase().includes(needle))
    : sessions;
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
      if (session) onPick(session.id);
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
            {sessions.length === 0
              ? "No saved sessions yet."
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
              const turns = session.items.filter(
                (item) => item.kind === "user",
              ).length;
              return (
                <li
                  key={session.id}
                  id={`session-${session.id}`}
                  role="option"
                  aria-selected={index === selected}
                  className={`session-row${index === selected ? " active" : ""}`}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => onPick(session.id)}
                >
                  <ClockCounterClockwiseIcon
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
      </div>
    </div>
  );
}
