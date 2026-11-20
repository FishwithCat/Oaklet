import { useEffect, useRef, useState } from "react";
import {
  CheckRegular,
  DownSmallRegular,
  GitBranch2Regular,
  Refresh3Regular,
} from "@mingcute/react/core-regular";
import { filterBranches } from "./git-model";

export function BranchMenu({
  active,
  branch,
  branches,
  loading,
  loadError,
  switchingBranch,
  branchError,
  onSwitch,
  onReload,
  onClearError,
  iconOnly = false,
}: {
  active: boolean;
  branch: string;
  branches: string[];
  loading: boolean;
  loadError: string;
  switchingBranch: string | null;
  branchError: string;
  onSwitch: (branch: string) => Promise<boolean>;
  onReload: () => void;
  onClearError: () => void;
  iconOnly?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const menuRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const reloadRef = useRef(onReload);
  reloadRef.current = onReload;
  const visible = filterBranches(branches, query);

  useEffect(() => {
    if (!open) {
      setQuery("");
      return;
    }

    searchRef.current?.focus();
    // Pick up branches created outside Oaklet since the menu was last opened.
    reloadRef.current();
  }, [open]);

  useEffect(() => {
    if (!open) return;

    function onPointerDown(event: PointerEvent) {
      if (
        !(event.target instanceof Node) ||
        !menuRef.current?.contains(event.target)
      ) {
        setOpen(false);
      }
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
        buttonRef.current?.focus();
      }
    }

    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (listRef.current) listRef.current.scrollTop = 0;
  }, [query]);

  useEffect(() => {
    if (!active) setOpen(false);
  }, [active]);

  async function choose(nextBranch: string) {
    if (nextBranch === branch) {
      setOpen(false);
      return;
    }
    if (await onSwitch(nextBranch)) {
      setOpen(false);
      buttonRef.current?.focus();
    }
  }

  function refresh() {
    onClearError();
    onReload();
  }

  return (
    <div className="sc-branch-wrap" ref={menuRef}>
      <button
        ref={buttonRef}
        type="button"
        className={`sc-branch${iconOnly ? " sc-branch-icon-only" : ""}`}
        onClick={() => {
          setOpen((current) => !current);
          onClearError();
        }}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? "sc-branch-menu" : undefined}
        aria-label={`Switch branch, current: ${branch || "detached"}`}
        title={`Switch branch, current: ${branch || "detached"}`}
      >
        {iconOnly ? (
          <GitBranch2Regular size={18} aria-hidden="true" />
        ) : (
          <>
            <GitBranch2Regular size={11} className="sc-branch-icon" aria-hidden="true" />
            <span className="sc-branch-name">{branch || "detached"}</span>
            <DownSmallRegular size={10} className="sc-branch-caret" />
          </>
        )}
      </button>

      {open && (
        <div
          id="sc-branch-menu"
          className="sc-branch-menu"
          role="dialog"
          aria-label="Switch branch"
        >
          <div className="sc-branch-menu-head">
            <div className="sc-branch-menu-title">Switch branch</div>
            <button
              type="button"
              className="sc-branch-refresh"
              onClick={refresh}
              disabled={loading || switchingBranch !== null}
              aria-label="Refresh branches"
              title="Refresh branches"
            >
              <Refresh3Regular size={12} className={loading ? "spin" : ""} />
            </button>
          </div>
          <input
            ref={searchRef}
            type="search"
            className="sc-branch-search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && visible.length > 0) {
                event.preventDefault();
                void choose(visible[0]);
              }
            }}
            placeholder="Search branches"
            aria-label="Search branches"
            spellCheck={false}
          />
          <div className="sc-branch-list" ref={listRef}>
            {branches.length === 0 ? (
              <p className="sc-branch-empty">No local branches.</p>
            ) : visible.length === 0 ? (
              <p className="sc-branch-empty">No matching branches.</p>
            ) : (
              visible.map((item) => (
                <button
                  type="button"
                  key={item}
                  className="sc-branch-item"
                  aria-current={item === branch ? "true" : undefined}
                  disabled={switchingBranch !== null || loading}
                  onClick={() => void choose(item)}
                  title={item}
                >
                  <span>{item}</span>
                  {switchingBranch === item ? (
                    <Refresh3Regular size={12} className="spin" />
                  ) : item === branch ? (
                    <CheckRegular size={11} />
                  ) : null}
                </button>
              ))
            )}
          </div>
          {(branchError || loadError) && (
            <p className="sc-branch-error">{branchError || loadError}</p>
          )}
        </div>
      )}
    </div>
  );
}
