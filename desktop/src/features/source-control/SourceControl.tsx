import {
  ArrowClockwiseIcon,
  CheckCircleIcon,
  GitBranchIcon,
} from "@phosphor-icons/react";
import { BranchMenu } from "./BranchMenu";
import { Diff } from "./Diff";
import {
  fullDate,
  relativeDate,
  statusInfo,
  type ChangeInfo,
} from "./git-model";
import { useSourceControl } from "./useSourceControl";

/** Uncommitted changes and the commit history, each with a file diff. */
export function SourceControl({
  active,
  refreshToken,
  onRefresh,
  changes,
  changesError,
}: {
  active: boolean;
  refreshToken: number;
  onRefresh: () => void;
  changes: ChangeInfo[] | null;
  changesError: string;
}) {
  const {
    tab,
    hasLog,
    setTab,
    commits,
    branches,
    branch,
    commit,
    change,
    selectedCommit,
    selectCommit,
    selectedChange,
    selectChange,
    patch,
    diffReady,
    diffError,
    loading,
    error,
    switchingBranch,
    branchError,
    clearBranchError,
    switchBranch,
    refresh,
  } = useSourceControl({ active, changes, refreshToken, onRefresh });

  return (
    <section className="sc" hidden={!active} aria-label="Source control">
      <header className="sc-head">
        <span className="sc-icon" aria-hidden="true">
          <GitBranchIcon size={14} />
        </span>
        <span className="sc-title">Source control</span>
        {hasLog && (
          <BranchMenu
            active={active}
            branch={branch}
            branches={branches}
            switchingBranch={switchingBranch}
            branchError={branchError}
            onSwitch={switchBranch}
            onClearError={clearBranchError}
          />
        )}
        <button
          type="button"
          className="ghost icon-button sc-refresh"
          onClick={refresh}
          disabled={loading}
          aria-label="Refresh source control"
          title="Refresh source control"
        >
          <ArrowClockwiseIcon size={14} className={loading ? "spin" : ""} />
        </button>
      </header>

      <div className="sc-tabs">
        <button
          type="button"
          className={`sc-tab${tab === "changes" ? " active" : ""}`}
          onClick={() => setTab("changes")}
          aria-pressed={tab === "changes"}
        >
          Changes
          {changes !== null && changes.length > 0 && (
            <span className="sc-tab-count">{changes.length}</span>
          )}
        </button>
        <button
          type="button"
          className={`sc-tab${tab === "commits" ? " active" : ""}`}
          onClick={() => setTab("commits")}
          aria-pressed={tab === "commits"}
        >
          Commits
        </button>
      </div>

      {error || changesError ? (
        <div className="sc-empty">
          <GitBranchIcon size={22} />
          <p>{error || changesError}</p>
          <button type="button" onClick={refresh}>
            Try again
          </button>
        </div>
      ) : loading || !hasLog || !changes ? (
        <p className="sc-note">Loading source control...</p>
      ) : tab === "changes" ? (
        changes.length === 0 ? (
          <div className="sc-empty">
            <CheckCircleIcon size={22} />
            <p>No uncommitted changes.</p>
          </div>
        ) : (
          <div className="sc-body">
            <div className="sc-list" aria-label="Uncommitted changes">
              {changes.map((entry) => {
                const info = statusInfo(entry.status);
                return (
                  <button
                    type="button"
                    key={entry.path}
                    className={`change${entry.path === selectedChange ? " active" : ""}`}
                    onClick={() => selectChange(entry.path)}
                    aria-current={entry.path === selectedChange}
                    title={entry.path}
                  >
                    <span className="change-path">{entry.path}</span>
                    <span
                      className={`change-status ${entry.status}`}
                      title={info.title}
                    >
                      {info.label}
                    </span>
                  </button>
                );
              })}
            </div>

            <div className="sc-detail">
              {change && (
                <div className="commit-head">
                  <h2>{change.path}</h2>
                  <p className="commit-meta-row">
                    <span className={`change-status ${change.status}`}>
                      {statusInfo(change.status).label}
                    </span>{" "}
                    {statusInfo(change.status).title} · uncommitted
                  </p>
                </div>
              )}
              <Diff
                text={patch}
                ready={diffReady}
                error={diffError}
                empty="No diff for this file."
              />
            </div>
          </div>
        )
      ) : commits.length === 0 ? (
        <div className="sc-empty">
          <GitBranchIcon size={22} />
          <p>No commits yet in this repository.</p>
        </div>
      ) : (
        <div className="sc-body">
          <div className="sc-list" aria-label="Recent commits">
            {commits.map((entry) => (
              <button
                type="button"
                key={entry.hash}
                className={`commit${entry.hash === selectedCommit ? " active" : ""}`}
                onClick={() => selectCommit(entry.hash)}
                aria-current={entry.hash === selectedCommit}
              >
                <span className="commit-subject">
                  {entry.subject || "Untitled commit"}
                </span>
                <span className="commit-meta">
                  <code>{entry.shortHash}</code>
                  <span className="commit-dot">·</span>
                  {entry.author}
                  <span className="commit-dot">·</span>
                  {relativeDate(entry.date)}
                </span>
              </button>
            ))}
          </div>

          <div className="sc-detail">
            {commit && (
              <div className="commit-head">
                <h2>{commit.subject || "Untitled commit"}</h2>
                {commit.body && <p className="commit-body">{commit.body}</p>}
                <p className="commit-meta-row">
                  <code>{commit.hash}</code>
                  <span>
                    {" "}
                    · {commit.author} &lt;{commit.email}&gt; ·{" "}
                  </span>
                  <span title={commit.date}>{fullDate(commit.date)}</span>
                </p>
              </div>
            )}

            <Diff
              text={patch}
              ready={diffReady}
              error={diffError}
              empty="No changes in this commit."
            />
          </div>
        </div>
      )}
    </section>
  );
}
