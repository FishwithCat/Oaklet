import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowClockwiseIcon,
  CaretDownIcon,
  CheckCircleIcon,
  CheckIcon,
  GitBranchIcon,
} from "@phosphor-icons/react";
import { pageHunks, parseDiff } from "./git-diff";

type CommitInfo = {
  hash: string;
  shortHash: string;
  author: string;
  email: string;
  date: string;
  subject: string;
  body: string;
};

type GitLog = { branch: string; branches: string[]; commits: CommitInfo[] };
export type ChangeInfo = { path: string; status: string; unstaged: boolean };

const CHANGE_STATUS: Record<string, { label: string; title: string }> = {
  untracked: { label: "U", title: "Untracked" },
  added: { label: "A", title: "Added" },
  modified: { label: "M", title: "Modified" },
  deleted: { label: "D", title: "Deleted" },
  renamed: { label: "R", title: "Renamed" },
  copied: { label: "C", title: "Copied" },
  typechange: { label: "T", title: "Type changed" },
  conflicted: { label: "!", title: "Conflicted" },
};

function statusInfo(status: string) {
  return CHANGE_STATUS[status] ?? { label: "M", title: "Modified" };
}

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}

function relativeDate(iso: string): string {
  const time = new Date(iso).getTime();
  if (Number.isNaN(time)) return iso;

  const elapsed = (Date.now() - time) / 1000;
  const units = [
    ["year", 31536000],
    ["month", 2592000],
    ["week", 604800],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ] as const;
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

  for (const [unit, seconds] of units) {
    if (Math.abs(elapsed) >= seconds) {
      return format.format(-Math.round(elapsed / seconds), unit);
    }
  }
  return "just now";
}

function fullDate(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString();
}

function Diff({
  text,
  ready,
  error,
  empty,
}: {
  text: string;
  ready: boolean;
  error: string;
  empty: string;
}) {
  const files = useMemo(() => parseDiff(text), [text]);
  const [fileIndex, setFileIndex] = useState(0);
  const [shown, setShown] = useState(200);
  useEffect(() => { setFileIndex(0); setShown(200); }, [text]);
  const selectedFile = files[fileIndex] ?? files[0];
  const lineCount = selectedFile?.hunks.reduce((sum, hunk) => sum + hunk.lines.length, 0) ?? 0;
  const added = files.reduce((total, file) => total + file.added, 0);
  const removed = files.reduce((total, file) => total + file.removed, 0);

  if (error) return <p className="sc-note error">{error}</p>;
  if (!ready) return <p className="sc-note">Loading diff...</p>;
  if (files.length === 0) return <p className="sc-note">{empty}</p>;

  return (
    <div className="diff">
      <div className="diff-summary">
        {files.length} file{files.length === 1 ? "" : "s"} changed
        <span className="diff-stat">
          <span className="add">+{added}</span>{" "}
          <span className="del">-{removed}</span>
        </span>
      </div>
      {files.length > 1 && (
        <label className="diff-file-picker">
          <span className="diff-file-picker-label">File</span>
          <span className="diff-file-picker-control">
            <select value={fileIndex} onChange={(event) => { setFileIndex(Number(event.target.value)); setShown(200); }}>
              {files.map((file, index) => <option key={index} value={index}>{file.path}</option>)}
            </select>
            <CaretDownIcon size={10} className="diff-file-picker-caret" aria-hidden="true" />
          </span>
        </label>
      )}
      {[selectedFile].map((file) => (
        <div className="diff-file" key={`${file.path}-${fileIndex}`}>
          <div className="diff-file-head">
            <span className="diff-path">{file.path}</span>
            <span className="diff-stat">
              <span className="add">+{file.added}</span>{" "}
              <span className="del">-{file.removed}</span>
            </span>
          </div>
          {file.binary ? (
            <p className="diff-binary">Binary file</p>
          ) : file.hunks.length === 0 ? (
            <p className="diff-binary">No line changes.</p>
          ) : (
            pageHunks(file.hunks, 0, shown).map((hunk, hunkIndex) => (
              <div className="diff-hunk" key={`${hunk.header}-${hunkIndex}`}>
                <div className="diff-hunk-head">{hunk.header}</div>
                <div className="diff-lines">
                  <div className="diff-lines-inner">
                    {hunk.lines.map((line, lineIndex) => (
                      <div
                        className={`diff-line ${line.kind}`}
                        key={lineIndex}
                      >
                        <span className="diff-num">{line.oldLine ?? ""}</span>
                        <span className="diff-num">{line.newLine ?? ""}</span>
                        <span className="diff-sign">
                          {line.kind === "add"
                            ? "+"
                            : line.kind === "del"
                              ? "-"
                              : " "}
                        </span>
                        <span className="diff-text">
                          {line.text === "" ? " " : line.text}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            ))
          )}
          {!file.binary && shown < lineCount && (
            <button type="button" className="diff-more" onClick={() => setShown((current) => current + 200)}>
              <span>Show more lines</span>
              <span className="diff-more-count">{shown} of {lineCount}</span>
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

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
  const [tab, setTab] = useState<"changes" | "commits">("changes");
  const [log, setLog] = useState<GitLog | null>(null);
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const [selectedChange, setSelectedChange] = useState<string | null>(null);
  const [patch, setPatch] = useState("");
  const [patchFor, setPatchFor] = useState<string | null>(null);
  const [diffError, setDiffError] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [branchOpen, setBranchOpen] = useState(false);
  const [switchingBranch, setSwitchingBranch] = useState<string | null>(null);
  const [branchError, setBranchError] = useState("");
  const branchMenuRef = useRef<HTMLDivElement>(null);
  const branchButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setError("");

    invoke<GitLog>("git_log", { includeCommits: tab === "commits" })
      .then((nextLog) => {
        if (cancelled) return;
        setLog(nextLog);
        if (tab === "commits") {
          setSelectedCommit((current) =>
            current && nextLog.commits.some((commit) => commit.hash === current)
              ? current
              : (nextLog.commits[0]?.hash ?? null),
          );
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setLog(null);
        setSelectedCommit(null);
        setError(errorText(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [active, tab, refreshToken]);

  useEffect(() => {
    if (!changes) return;
    setSelectedChange((current) =>
      current && changes.some((change) => change.path === current)
        ? current
        : (changes[0]?.path ?? null),
    );
  }, [changes]);

  useEffect(() => {
    if (!branchOpen) return;

    function onPointerDown(event: PointerEvent) {
      if (
        !(event.target instanceof Node) ||
        !branchMenuRef.current?.contains(event.target)
      ) {
        setBranchOpen(false);
      }
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setBranchOpen(false);
        branchButtonRef.current?.focus();
      }
    }

    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [branchOpen]);

  useEffect(() => {
    if (!active) setBranchOpen(false);
  }, [active]);

  const target = tab === "commits" ? selectedCommit : selectedChange;
  const targetKey = target === null ? null : `${tab}:${target}`;

  useEffect(() => {
    if (!active || !target) {
      setPatch("");
      setPatchFor(null);
      setDiffError("");
      return;
    }

    let cancelled = false;
    setPatch("");
    setPatchFor(null);
    setDiffError("");
    const key = `${tab}:${target}`;

    const request =
      tab === "commits"
        ? invoke<string>("git_show", { hash: target })
        : invoke<string>("git_diff", { path: target });

    request
      .then((text) => {
        if (cancelled) return;
        setPatch(text);
        setPatchFor(key);
      })
      .catch((err) => {
        if (cancelled) return;
        setDiffError(errorText(err));
        setPatchFor(key);
      });

    return () => {
      cancelled = true;
    };
  }, [active, tab, target, refreshToken]);

  const commits = log?.commits ?? [];
  const branches = log?.branches ?? [];
  const commit = commits.find((entry) => entry.hash === selectedCommit) ?? null;
  const change =
    changes?.find((entry) => entry.path === selectedChange) ?? null;
  const diffReady = targetKey !== null && patchFor === targetKey;

  function refresh() {
    onRefresh();
  }

  async function switchBranch(branch: string) {
    if (!log || switchingBranch) return;
    if (branch === log.branch) {
      setBranchOpen(false);
      return;
    }

    setSwitchingBranch(branch);
    setBranchError("");
    try {
      await invoke("git_switch", { branch });
      setBranchOpen(false);
      refresh();
    } catch (err) {
      setBranchError(errorText(err));
    } finally {
      setSwitchingBranch(null);
    }
  }

  return (
    <section className="sc" hidden={!active} aria-label="Source control">
      <header className="sc-head">
        <span className="sc-icon" aria-hidden="true">
          <GitBranchIcon size={14} />
        </span>
        <span className="sc-title">Source control</span>
        {log && (
          <div className="sc-branch-wrap" ref={branchMenuRef}>
            <button
              ref={branchButtonRef}
              type="button"
              className="sc-branch"
              onClick={() => {
                setBranchOpen((open) => !open);
                setBranchError("");
              }}
              aria-haspopup="menu"
              aria-expanded={branchOpen}
              aria-controls={branchOpen ? "sc-branch-menu" : undefined}
              aria-label={`Switch branch, current: ${log.branch || "detached"}`}
              title="Switch branch"
            >
              <span className="sc-branch-name">
                {log.branch || "detached"}
              </span>
              <CaretDownIcon size={10} className="sc-branch-caret" />
            </button>

            {branchOpen && (
              <div
                id="sc-branch-menu"
                className="sc-branch-menu"
                role="menu"
                aria-label="Switch branch"
              >
                <div className="sc-branch-menu-title">Switch branch</div>
                {branches.length === 0 ? (
                  <p className="sc-branch-empty">No local branches.</p>
                ) : (
                  branches.map((branch) => (
                    <button
                      type="button"
                      role="menuitemradio"
                      key={branch}
                      className="sc-branch-item"
                      aria-checked={branch === log.branch}
                      disabled={switchingBranch !== null}
                      onClick={() => switchBranch(branch)}
                      title={branch}
                    >
                      <span>{branch}</span>
                      {switchingBranch === branch ? (
                        <ArrowClockwiseIcon size={12} className="spin" />
                      ) : branch === log.branch ? (
                        <CheckIcon size={11} weight="bold" />
                      ) : null}
                    </button>
                  ))
                )}
                {branchError && (
                  <p className="sc-branch-error">{branchError}</p>
                )}
              </div>
            )}
          </div>
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
      ) : loading || !log || !changes ? (
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
                    onClick={() => setSelectedChange(entry.path)}
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
                onClick={() => setSelectedCommit(entry.hash)}
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
