import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../shared/error-text";

/**
 * The backend owns the workspace-to-window binding. Reading it here (instead
 * of the last-used config value) keeps every window on its own project even
 * when another window opens a different folder.
 */
export function useWindowWorkspace(enabled: boolean) {
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    invoke<string>("window_workspace")
      .then((next) => {
        if (!cancelled) setWorkspace(next);
      })
      .catch((reason) => {
        if (!cancelled) setError(errorText(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  return { workspace, error };
}
