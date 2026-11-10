import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ChangeInfo } from "./git-model";

/**
 * The single source of uncommitted changes. App calls this once so the sidebar
 * badge and the source-control view always agree.
 */
export function useGitChanges({
  enabled,
  workspace,
  refreshToken,
}: {
  enabled: boolean;
  workspace: string;
  refreshToken: number;
}) {
  const [changes, setChanges] = useState<ChangeInfo[] | null>(null);
  const [changesError, setChangesError] = useState("");

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setChanges(null);
    setChangesError("");
    invoke<ChangeInfo[]>("git_changes")
      .then((next) => {
        if (!cancelled) setChanges(next);
      })
      .catch((error) => {
        if (!cancelled) setChangesError(errorText(error));
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, workspace, refreshToken]);

  const unstaged = useMemo(
    () => changes?.filter((change) => change.unstaged).length ?? null,
    [changes],
  );

  return { changes, changesError, unstaged };
}
