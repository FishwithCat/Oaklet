import { useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import {
  CheckCircleRegular,
  GitBranch2Regular,
  Refresh3Regular,
} from "@mingcute/react/core-regular";
import { PaneResizer } from "../../shared/PaneResizer";
import { CommitDetails } from "./CommitDetails";
import { CommitPicker } from "./CommitPicker";
import { Diff, type DiffEdge } from "./Diff";
import { filterReviewFiles, statusInfo, type ChangeInfo } from "./git-model";
import { useSourceControl } from "./useSourceControl";

export function SourceControl({
  active,
  workspace,
  refreshToken,
  onRefresh,
  changes,
  changesError,
  changesReady,
}: {
  active: boolean;
  workspace: string;
  refreshToken: number;
  onRefresh: () => void;
  changes: ChangeInfo[] | null;
  changesError: string;
  changesReady: boolean;
}) {
  const {
    tab, setTab, commits, commit, setSelectedCommit, files, selection, target,
    patch, diffError, loading, error, loadingFiles, filesError,
  } = useSourceControl({ active, workspace, changes, changesReady, refreshToken });
  const [listWidth, setListWidth] = useState(300);
  const [filters, setFilters] = useState({ changes: "", commits: "" });
  const [landing, setLanding] = useState<DiffEdge | null>(null);
  const detail = useRef<HTMLDivElement>(null);
  const fileList = useRef<HTMLDivElement>(null);
  const focusedRow = useRef<HTMLButtonElement | null>(null);
  const bodyStyle = { "--sc-list-width": `${listWidth}px` } as CSSProperties;
  const query = filters[tab];
  const visible = filterReviewFiles(files ?? [], query);
  const selectedPath = selection.file?.path;
  const index = visible.findIndex((file) => file.path === selectedPath);
  const previous = visible[index - 1];
  const next = index >= 0 ? visible[index + 1] : visible[0];
  const loadError = tab === "changes" ? changesError : error;

  useLayoutEffect(() => {
    if (!active || !selectedPath) return;
    const row = fileList.current?.querySelector<HTMLButtonElement>('[aria-current="true"]');
    if (landing) row?.scrollIntoView({ block: "nearest" });
    if (focusedRow.current && !focusedRow.current.isConnected && document.activeElement === document.body) {
      row?.focus();
      focusedRow.current = row ?? null;
    }
  }, [active, landing, selectedPath]);

  function selectFile(path: string, edge: DiffEdge | null = null) {
    setLanding(edge);
    selection.select(path);
    if (edge) detail.current?.focus({ preventScroll: true });
  }

  function moveFocus(event: KeyboardEvent<HTMLButtonElement>, row: number) {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const destinations: Record<string, number> = {
      ArrowUp: Math.max(row - 1, 0),
      ArrowDown: Math.min(row + 1, visible.length - 1),
      Home: 0,
      End: visible.length - 1,
    };
    const destination = destinations[event.key];
    if (destination === undefined) return;
    event.preventDefault();
    selectFile(visible[destination].path);
    const button = event.currentTarget.parentElement?.querySelectorAll("button")[destination];
    button?.focus({ preventScroll: true });
    button?.scrollIntoView({ block: "nearest" });
  }

  return (
    <section className="sc" hidden={!active} aria-label="Source control">
      <header className="view-bar tab-bar">
        <span className="view-bar-icon" aria-hidden="true">
          <GitBranch2Regular size={14} />
        </span>
        <button
          type="button"
          className={`view-tab${tab === "changes" ? " active" : ""}`}
          onClick={() => { setLanding(null); setTab("changes"); }}
          aria-pressed={tab === "changes"}
        >
          Changes
          {!!changes?.length && <span className="sc-tab-count">{changes.length}</span>}
        </button>
        <button
          type="button"
          className={`view-tab${tab === "commits" ? " active" : ""}`}
          onClick={() => { setLanding(null); setTab("commits"); }}
          aria-pressed={tab === "commits"}
        >
          Commits
        </button>
        <button
          type="button"
          className="ghost icon-button view-bar-refresh"
          onClick={onRefresh}
          disabled={loading}
          aria-label="Refresh source control"
          title="Refresh source control"
        >
          <Refresh3Regular size={14} className={loading ? "spin" : ""} />
        </button>
      </header>

      {loadError && (
        <p className="sc-note error" role="status">
          {loadError} <button type="button" onClick={onRefresh}>Try again</button>
        </p>
      )}
      <div className="sc-body" style={bodyStyle}>
        <aside className="sc-list" aria-label={tab === "changes" ? "Uncommitted changes" : "Commit changes"}>
          {tab === "commits" && commit && (
            <div className="sc-commit-info">
              <CommitPicker
                commits={commits}
                commit={commit}
                onSelect={(hash) => { setLanding(null); setSelectedCommit(hash); }}
              />
              <CommitDetails key={commit.hash} commit={commit} />
            </div>
          )}
          <input
            type="search"
            className="sc-control sc-file-filter"
            aria-label="Filter changed files by path"
            placeholder="Filter files by path"
            value={query}
            onChange={(event) => setFilters({ ...filters, [tab]: event.target.value })}
          />
          {tab === "commits" && filesError && <p className="sc-note error" role="status">{filesError}</p>}
          <div className="sc-file-list" aria-label="Changed files" ref={fileList}>
            {visible.map((entry, row) => {
              const info = statusInfo(entry.status);
              const selected = entry.path === selectedPath;
              const separator = entry.path.lastIndexOf("/");
              return (
                <button
                  type="button"
                  key={entry.path}
                  className={`change${selected ? " active" : ""}`}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true });
                    selectFile(entry.path);
                  }}
                  onKeyDown={(event) => moveFocus(event, row)}
                  onFocus={(event) => { focusedRow.current = event.currentTarget; }}
                  onBlur={(event) => { if (event.relatedTarget) focusedRow.current = null; }}
                  tabIndex={selected || (index === -1 && row === 0) ? 0 : -1}
                  aria-current={selected ? "true" : undefined}
                  title={entry.oldPath ? `${entry.oldPath} → ${entry.path}` : entry.path}
                >
                  <span className="change-path">
                    <span className="change-name">{entry.path.slice(separator + 1)}</span>
                    {separator > 0 && <span className="change-directory">{entry.path.slice(0, separator)}</span>}
                    {entry.oldPath && <span className="change-previous">From {entry.oldPath}</span>}
                  </span>
                  {entry.added != null && entry.removed != null && (entry.added !== 0 || entry.removed !== 0) && (
                    <span className="diff-stat"><span className="add">+{entry.added}</span><span className="del">−{entry.removed}</span></span>
                  )}
                  <span className={`change-status ${entry.status}`} title={info.title} aria-label={info.title}>{info.label}</span>
                </button>
              );
            })}
            {!!files?.length && visible.length === 0 && <p className="sc-note">No matching files.</p>}
          </div>
        </aside>

        <PaneResizer width={listWidth} label="Resize changed files" onChange={setListWidth} />

        <div className="sc-detail" ref={detail} tabIndex={-1}>
          {target && selection.file ? (
            <Diff
              key={JSON.stringify([workspace, tab, target, refreshToken])}
              target={target}
              text={patch}
              error={tab === "changes" && changesError ? "Refresh to load this diff." : diffError}
              landing={landing}
              onPreviousFile={previous ? () => selectFile(previous.path, "last") : undefined}
              onNextFile={next ? () => selectFile(next.path, "first") : undefined}
            />
          ) : tab === "changes" && files?.length === 0 ? (
            <div className="sc-empty"><CheckCircleRegular size={22} /><p>No uncommitted changes.</p></div>
          ) : tab === "commits" && !loadError && !loading && commits.length === 0 ? (
            <div className="sc-empty"><GitBranch2Regular size={22} /><p>No commits yet in this repository.</p></div>
          ) : tab === "commits" && files?.length === 0 ? (
            <p className="sc-note">No changes in this commit.</p>
          ) : !loadError && !filesError && (!files || loadingFiles) ? (
            <p className="sc-note">Loading changed files...</p>
          ) : null}
        </div>
      </div>
    </section>
  );
}
