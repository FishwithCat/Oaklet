import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorText } from "../../shared/error-text";

export type WorktreeInfo = {
  directory: string;
  head: { branch: string | null; commit: string | null } | null;
  checkout: { path: string; source: string; branch: string | null; baseCommit: string | null } | null;
  missing: boolean;
  waitingText: string | null;
};
type Progress = { sessionId: string; phase: string };

export function useSessionWorktrees(ids: string[], revision: number) {
  const [infos, setInfos] = useState<Record<string, WorktreeInfo>>({});
  const [phases, setPhases] = useState<Record<string, string>>({});
  const [refresh, setRefresh] = useState(0);
  const key = ids.join("\n");
  const reload = useCallback(() => setRefresh((value) => value + 1), []);
  const setPhase = useCallback((sessionId: string, phase: string) => {
    setPhases((values) => ({ ...values, [sessionId]: phase }));
  }, []);

  useEffect(() => {
    let disposed = false;
    const found: [string, WorktreeInfo][] = [];
    void Promise.all(ids.map(async (id) => {
      try {
        found.push([id, await invoke<WorktreeInfo>("session_worktree", { sessionId: id })]);
      } catch (error) {
        console.error("Could not read a session checkout", error);
      }
    })).then(() => {
      if (!disposed) setInfos(Object.fromEntries(found));
    });
    return () => { disposed = true; };
  }, [key, refresh, revision]);

  useEffect(() => {
    let disposed = false;
    const releases: (() => void)[] = [];
    async function follow(release: Promise<() => void>) {
      const unlisten = await release;
      if (disposed) unlisten();
      else releases.push(unlisten);
    }
    void Promise.all([
      follow(listen<Progress>("worktree://progress", ({ payload }) => setPhase(payload.sessionId, payload.phase))),
      follow(listen("worktree://changed", reload)),
    ]).catch(console.error);
    window.addEventListener("focus", reload);
    return () => {
      disposed = true;
      releases.forEach((release) => release());
      window.removeEventListener("focus", reload);
    };
  }, [reload, setPhase]);

  return { infos, phases, setPhase, reload };
}

const PROGRESS_LABELS: Record<string, string> = {
  prepare: "Preparing the worktree…",
  checkout: "Checking out files…",
};

export function WorktreeControl({
  info, phase, retry, stop, recover,
}: {
  info: WorktreeInfo;
  phase: string;
  retry: (keepRequest: boolean) => void;
  stop: () => void;
  recover: (restore: boolean) => Promise<boolean>;
}) {
  const [recovery, setRecovery] = useState("");
  const [recovering, setRecovering] = useState(false);
  const canRecover = info.checkout?.baseCommit;

  async function recoverCheckout(restore: boolean) {
    setRecovering(true);
    setRecovery("");
    try {
      if ((await recover(restore)) && info.waitingText !== null) retry(true);
    } catch (error) {
      setRecovery(errorText(error));
    } finally {
      setRecovering(false);
    }
  }

  if (!phase && !info.missing && info.waitingText === null) return null;

  return (
    <div className="worktree-control">
      {phase && <div className="worktree-row" role="status">
        <span>{PROGRESS_LABELS[phase] ?? phase}</span>
        <button type="button" onClick={stop}>Cancel</button>
      </div>}
      {info.missing && (
        <div className="worktree-recovery">
          <p role="status">This chat’s worktree is missing. Restore it or continue in its source checkout.</p>
          {canRecover && <button type="button" disabled={recovering} onClick={() => void recoverCheckout(true)}>Restore worktree</button>}
          <button type="button" disabled={recovering} onClick={() => void recoverCheckout(false)}>Continue in the project</button>
          {recovery && <p role="alert">{recovery}</p>}
        </div>
      )}
      {info.waitingText !== null && !phase && (
        <p role="status">Waiting message: {info.waitingText || "Message with attachments"}</p>
      )}
      {info.waitingText !== null && !phase && !info.missing && (
        <div className="worktree-recovery">
          <button type="button" disabled={recovering} onClick={() => retry(true)}>Retry</button>
          <button type="button" disabled={recovering} onClick={() => retry(false)}>Continue without a worktree</button>
        </div>
      )}
    </div>
  );
}
