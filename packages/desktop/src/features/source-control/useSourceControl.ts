import { useEffect, useLayoutEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import { chooseReviewPath, sortReviewFiles, type ChangeInfo, type GitLog, type ReviewFile } from "./git-model";

type RequestArgs = Record<string, unknown>;
const LOG_ARGS = { includeCommits: true };

export type ReviewTarget = {
  path: string;
  hash: string | null;
  oldPath: string | null;
  status: string;
};

function useRequest<T>(command: string, args: RequestArgs | null, revision = 0) {
  const [result, setResult] = useState<{
    args: RequestArgs; revision: number; value: T | null; error: string;
  } | null>(null);

  useEffect(() => {
    if (!args) return;
    let cancelled = false;
    invoke<T>(command, args)
      .then((value) => {
        if (!cancelled) setResult({ args, revision, value, error: "" });
      })
      .catch((error) => {
        if (!cancelled) setResult({ args, revision, value: null, error: errorText(error) });
      });
    return () => { cancelled = true; };
  }, [command, args, revision]);

  const current = result?.args === args ? result : null;
  const ready = current?.revision === revision;
  return { value: current?.value ?? null, error: ready ? current.error : "", ready };
}

function useSelection(files: ReviewFile[] | null) {
  const [selected, select] = useState<string | null>(null);
  const path = files ? chooseReviewPath(files, selected) : null;
  const file = files?.find((entry) => entry.path === path) ?? null;

  useLayoutEffect(() => {
    if (files && path !== selected) select(path);
  }, [files, path, selected]);

  return { file, select };
}

export function useSourceControl({
  active,
  changes,
  changesReady,
  refreshToken,
}: {
  active: boolean;
  changes: ChangeInfo[] | null;
  changesReady: boolean;
  refreshToken: number;
}) {
  const [tab, setTab] = useState<"changes" | "commits">("changes");
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const log = useRequest<GitLog>("git_log", active && tab === "commits" ? LOG_ARGS : null, refreshToken);
  const commits = log.value?.commits ?? [];
  const commit = commits.find((entry) => entry.hash === selectedCommit) ?? commits[0] ?? null;
  const commitHash = commit?.hash;
  useLayoutEffect(() => {
    if (commitHash && commitHash !== selectedCommit) setSelectedCommit(commitHash);
  }, [commitHash, selectedCommit]);
  const commitArgs = useMemo(() => commitHash ? { hash: commitHash } : null, [commitHash]);
  const commitFiles = useRequest<ReviewFile[]>("git_commit_files", commitArgs, refreshToken);
  const committed = useMemo(
    () => commitFiles.value && sortReviewFiles(commitFiles.value),
    [commitFiles.value],
  );
  const uncommitted = useMemo(() => changes && sortReviewFiles(changes), [changes]);
  const change = useSelection(uncommitted);
  const history = useSelection(committed);
  const selection = tab === "changes" ? change : history;
  const files = tab === "changes" ? uncommitted : committed;
  const path = selection.file?.path;
  const oldPath = selection.file?.oldPath ?? null;
  const status = selection.file?.status;
  const hash = tab === "commits" ? (commitHash ?? null) : null;
  const target = useMemo<ReviewTarget | null>(
    () => active && path && status ? { path, hash, oldPath, status } : null,
    [active, path, hash, oldPath, status],
  );
  const canLoadPatch = tab === "commits" || changesReady;
  const patchArgs = useMemo(() => target && canLoadPatch ? { target } : null, [target, canLoadPatch]);
  const patch = useRequest<string>("git_review_diff", patchArgs, refreshToken);

  return {
    tab, setTab, commits, commit, setSelectedCommit, files, selection, target,
    patch: patch.ready ? patch.value : null,
    diffError: patch.error,
    error: tab === "commits" ? log.error : "",
    loading: tab === "commits" && !log.ready,
    loadingFiles: tab === "commits" && !commitFiles.ready,
    filesError: commitFiles.error,
  };
}
