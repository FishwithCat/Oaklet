import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import type { GitLog } from "./git-model";

/**
 * The current branch and local branches for a view header, plus the switch
 * action. The list comes from git_log so every view sees the same branches.
 */
export function useGitBranch({
  active,
  refreshToken,
  onRefresh,
}: {
  active: boolean;
  refreshToken: number;
  onRefresh: () => void;
}) {
  const [loaded, setLoaded] = useState<GitLog | null>(null);
  const [loadError, setLoadError] = useState("");
  const [switchingBranch, setSwitchingBranch] = useState<string | null>(null);
  const [branchError, setBranchError] = useState("");

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoadError("");
    invoke<GitLog>("git_log", { includeCommits: false })
      .then((log) => {
        if (!cancelled) setLoaded(log);
      })
      .catch((error) => {
        if (!cancelled) {
          setLoaded(null);
          setLoadError(errorText(error));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [active, refreshToken]);

  async function switchBranch(nextBranch: string): Promise<boolean> {
    if (!loaded || switchingBranch) return false;
    if (nextBranch === loaded.branch) return true;

    setSwitchingBranch(nextBranch);
    setBranchError("");
    try {
      await invoke("git_switch", { branch: nextBranch });
      onRefresh();
      return true;
    } catch (error) {
      setBranchError(errorText(error));
      return false;
    } finally {
      setSwitchingBranch(null);
    }
  }

  return {
    branch: loaded?.branch ?? "",
    branches: loaded?.branches ?? [],
    branchLoaded: loaded !== null,
    loadError,
    switchingBranch,
    branchError,
    switchBranch,
    clearBranchError: () => setBranchError(""),
  };
}
