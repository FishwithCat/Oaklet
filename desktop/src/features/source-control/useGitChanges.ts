import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ChangeInfo } from "./git-model";

type LoadedChanges = { workspace: string; changes: ChangeInfo[] };

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
  const [loaded, setLoaded] = useState<LoadedChanges | null>(null);
  const [changesError, setChangesError] = useState("");
  // Keep the previous result while a refresh is in flight so the badge does not
  // unmount between polls; a different workspace still reads as unknown.
  const changes = loaded?.workspace === workspace ? loaded.changes : null;

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setChangesError("");
    invoke<ChangeInfo[]>("git_changes")
      .then((next) => {
        if (!cancelled) setLoaded({ workspace, changes: next });
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
