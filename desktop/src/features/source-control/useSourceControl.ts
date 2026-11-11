import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ChangeInfo, GitLog } from "./git-model";
import { useGitBranch } from "./useGitBranch";

export function useSourceControl({
  active,
  changes,
  refreshToken,
  onRefresh,
}: {
  active: boolean;
  changes: ChangeInfo[] | null;
  refreshToken: number;
  onRefresh: () => void;
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
  const {
    branch,
    branches,
    branchLoaded,
    loadError,
    switchingBranch,
    branchError,
    switchBranch,
    clearBranchError,
  } = useGitBranch({ active, refreshToken, onRefresh });

  useEffect(() => {
    if (!active || tab !== "commits") {
      setLoading(false);
      setError("");
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError("");

    invoke<GitLog>("git_log", { includeCommits: true })
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
  const commit = commits.find((entry) => entry.hash === selectedCommit) ?? null;
  const change =
    changes?.find((entry) => entry.path === selectedChange) ?? null;
  const diffReady = targetKey !== null && patchFor === targetKey;

  return {
    tab,
    setTab,
    hasLog: branchLoaded,
    commits,
    branches,
    branch,
    commit,
    change,
    selectedCommit,
    selectCommit: setSelectedCommit,
    selectedChange,
    selectChange: setSelectedChange,
    patch,
    diffReady,
    diffError,
    loading,
    error: error || loadError,
    switchingBranch,
    branchError,
    clearBranchError,
    switchBranch,
    refresh: onRefresh,
  };
}
