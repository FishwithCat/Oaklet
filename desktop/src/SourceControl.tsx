import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowClockwiseIcon,
  GitBranchIcon,
} from "@phosphor-icons/react";
import { parseDiff } from "./git-diff";

type CommitInfo = {
  hash: string;
  shortHash: string;
  author: string;
  email: string;
  date: string;
  subject: string;
  body: string;
};

type GitLog = { branch: string; commits: CommitInfo[] };

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

/** Commit history and the diff of the selected commit. */
export function SourceControl({
  active,
  refreshToken = 0,
  onRefresh,
}: {
  active: boolean;
  refreshToken?: number;
  onRefresh?: () => void;
}) {
  const [log, setLog] = useState<GitLog | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [patch, setPatch] = useState("");
  const [patchFor, setPatchFor] = useState<string | null>(null);
  const [diffError, setDiffError] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setError("");

    invoke<GitLog>("git_log")
      .then((next) => {
        if (cancelled) return;
        setLog(next);
        setSelected((current) =>
          current && next.commits.some((commit) => commit.hash === current)
            ? current
            : (next.commits[0]?.hash ?? null),
        );
      })
      .catch((err) => {
        if (cancelled) return;
        setLog(null);
        setSelected(null);
        setError(errorText(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [active, reload, refreshToken]);

  useEffect(() => {
    if (!active || !selected) {
      setPatch("");
      setPatchFor(null);
      setDiffError("");
      return;
    }

    let cancelled = false;
    setPatch("");
    setPatchFor(null);
    setDiffError("");

    invoke<string>("git_show", { hash: selected })
      .then((text) => {
        if (cancelled) return;
        setPatch(text);
        setPatchFor(selected);
      })
      .catch((err) => {
        if (cancelled) return;
        setDiffError(errorText(err));
        setPatchFor(selected);
      });

    return () => {
      cancelled = true;
    };
  }, [active, selected, reload]);

  const commits = log?.commits ?? [];
  const commit = commits.find((entry) => entry.hash === selected) ?? null;
  const files = useMemo(() => parseDiff(patch), [patch]);
  const added = files.reduce((total, file) => total + file.added, 0);
  const removed = files.reduce((total, file) => total + file.removed, 0);
  const diffReady = patchFor === selected;

  function refresh() {
    setReload((value) => value + 1);
    onRefresh?.();
  }

  return (
    <section className="sc" hidden={!active} aria-label="Source control">
      <header className="sc-head">
        <span className="sc-icon" aria-hidden="true">
          <GitBranchIcon size={14} />
        </span>
        <span className="sc-title">Source control</span>
        {log?.branch && <span className="sc-branch">{log.branch}</span>}
        <button
          type="button"
          className="ghost icon-button sc-refresh"
          onClick={refresh}
          disabled={loading}
          aria-label="Refresh commits"
          title="Refresh commits"
        >
          <ArrowClockwiseIcon size={14} className={loading ? "spin" : ""} />
        </button>
      </header>

      {error ? (
        <div className="sc-empty">
          <GitBranchIcon size={22} />
          <p>{error}</p>
          <button type="button" onClick={refresh}>
            Try again
          </button>
        </div>
      ) : !log ? (
        <p className="sc-note">Loading commits...</p>
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
                className={`commit${entry.hash === selected ? " active" : ""}`}
                onClick={() => setSelected(entry.hash)}
                aria-current={entry.hash === selected}
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

            {diffError ? (
              <p className="sc-note error">{diffError}</p>
            ) : !diffReady ? (
              <p className="sc-note">Loading diff...</p>
            ) : files.length === 0 ? (
              <p className="sc-note">No changes in this commit.</p>
            ) : (
              <div className="diff">
                <div className="diff-summary">
                  {files.length} file{files.length === 1 ? "" : "s"} changed
                  <span className="diff-stat">
                    <span className="add">+{added}</span>{" "}
                    <span className="del">-{removed}</span>
                  </span>
                </div>
                {files.map((file, fileIndex) => (
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
                      file.hunks.map((hunk, hunkIndex) => (
                        <div className="diff-hunk" key={`${hunk.header}-${hunkIndex}`}>
                          <div className="diff-hunk-head">{hunk.header}</div>
                          <div className="diff-lines">
                            <div className="diff-lines-inner">
                              {hunk.lines.map((line, lineIndex) => (
                                <div
                                  className={`diff-line ${line.kind}`}
                                  key={lineIndex}
                                >
                                  <span className="diff-num">
                                    {line.oldLine ?? ""}
                                  </span>
                                  <span className="diff-num">
                                    {line.newLine ?? ""}
                                  </span>
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
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
