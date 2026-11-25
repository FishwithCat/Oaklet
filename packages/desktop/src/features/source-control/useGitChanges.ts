import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { ChangeInfo } from "./git-model";

type LoadedChanges = { workspace: string; revision: number; changes: ChangeInfo[] };

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
  const [failure, setFailure] = useState<{ workspace: string; revision: number; message: string } | null>(null);
  // Keep the previous result while a refresh is in flight so the badge does not
  // unmount between polls; a different workspace still reads as unknown.
  const changes = loaded?.workspace === workspace ? loaded.changes : null;

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    invoke<ChangeInfo[]>("git_changes")
      .then((next) => {
        if (!cancelled) {
          setLoaded({ workspace, revision: refreshToken, changes: next });
          setFailure(null);
        }
      })
      .catch((error) => {
        if (!cancelled) setFailure({ workspace, revision: refreshToken, message: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, workspace, refreshToken]);

  const changesReady = !!changes && loaded?.revision === refreshToken;
  const changesError = failure?.workspace === workspace && failure.revision === refreshToken ? failure.message : "";
  return { changes, changeCount: changes?.length ?? null, changesError, changesReady };
}
