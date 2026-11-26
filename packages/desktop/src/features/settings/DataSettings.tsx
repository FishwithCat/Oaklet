import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import {
  formatBytes,
  formatCount,
  sessionsLabel,
  totalBytes,
  type PurgeResult,
  type StorageUsage,
} from "./data-model";

type Scope = { workspace: string | null };

export function DataSettings() {
  const [usage, setUsage] = useState<StorageUsage | null>(null);
  const [error, setError] = useState("");
  const [status, setStatus] = useState("");
  const [confirming, setConfirming] = useState<Scope | null>(null);
  const [purging, setPurging] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(() => {
    invoke<StorageUsage>("storage_usage")
      .then((next) => {
        if (!mounted.current) return;
        setUsage(next);
        setError("");
      })
      .catch((failure) => {
        if (mounted.current) setError(errorText(failure));
      });
  }, []);

  useEffect(load, [load]);

  async function purge(workspace: string | null) {
    setPurging(true);
    setError("");
    setStatus("");
    try {
      const result = await invoke<PurgeResult>("storage_purge", { workspace });
      if (!mounted.current) return;
      setUsage(result.usage);
      setConfirming(null);
      setStatus(
        result.deleted === 0
          ? "Nothing to delete."
          : `Deleted ${sessionsLabel(result.deleted)}.`,
      );
    } catch (failure) {
      if (!mounted.current) return;
      setError(errorText(failure));
      setConfirming(null);
      load();
    } finally {
      if (mounted.current) setPurging(false);
    }
  }

  if (!usage) {
    return error ? (
      <p className="error-text" role="alert">
        {error}
      </p>
    ) : (
      <p className="field-hint">Reading stored history...</p>
    );
  }

  const hasHistory = usage.sessions > 0 || usage.spillFiles > 0;

  return (
    <div className="data-settings">
      <div className="data-summary">
        <div className="data-stat">
          <span className="data-stat-value">{formatBytes(totalBytes(usage))}</span>
          <span className="data-stat-label">On disk</span>
        </div>
        <div className="data-stat">
          <span className="data-stat-value">
            {formatBytes(usage.databaseBytes)}
          </span>
          <span className="data-stat-label">Session database</span>
        </div>
        <div className="data-stat">
          <span className="data-stat-value">{formatCount(usage.sessions)}</span>
          <span className="data-stat-label">Saved sessions</span>
        </div>
        <div className="data-stat">
          <span className="data-stat-value">{formatCount(usage.messages)}</span>
          <span className="data-stat-label">Messages</span>
        </div>
      </div>

      <p className="field-hint">
        History stays on this machine. Spilled tool output adds{" "}
        {formatBytes(usage.spillBytes)} in {formatCount(usage.spillFiles)}{" "}
        {usage.spillFiles === 1 ? "file" : "files"}.
      </p>

      {error && (
        <p className="error-text data-status" role="alert">
          {error}
        </p>
      )}
      {status && (
        <p className="data-status" role="status">
          {status}
        </p>
      )}

      <h3 className="data-heading">Stored per workspace</h3>
      {usage.workspaces.length === 0 ? (
        <p className="data-empty">No session history is stored yet.</p>
      ) : (
        <ul className="data-workspaces">
          {usage.workspaces.map((row) => {
            const confirmingRow =
              confirming !== null && confirming.workspace === row.workspace;
            return (
              <li key={row.workspace} className="data-workspace">
                <div className="data-workspace-main">
                  <span className="data-workspace-path" title={row.label || row.workspace}>
                    {row.label || row.workspace}
                  </span>
                  <span className="data-workspace-meta">
                    {sessionsLabel(row.sessions)} ·{" "}
                    {formatCount(row.messages)} messages ·{" "}
                    {formatBytes(row.bytes)} of text
                    {row.updatedAt > 0 &&
                      ` · last used ${new Date(row.updatedAt).toLocaleDateString()}`}
                  </span>
                </div>
                {confirmingRow ? (
                  <div className="data-confirm">
                    <button
                      type="button"
                      className="danger"
                      autoFocus
                      onClick={() => void purge(row.workspace)}
                      disabled={purging}
                    >
                      {purging ? "Deleting..." : "Confirm delete"}
                    </button>
                    <button
                      type="button"
                      onClick={() => setConfirming(null)}
                      disabled={purging}
                    >
                      Cancel
                    </button>
                  </div>
                ) : (
                  <button
                    type="button"
                    onClick={() => {
                      setConfirming({ workspace: row.workspace });
                      setStatus("");
                    }}
                    disabled={purging}
                  >
                    Delete
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <div className="data-danger">
        <div className="data-danger-text">
          <strong>Delete all history</strong>
          <p className="field-hint">
            Removes every saved session in every workspace and its spilled tool
            output. Open chats start over. This cannot be undone.
          </p>
        </div>
        {confirming !== null && confirming.workspace === null ? (
          <div className="data-confirm">
            <button
              type="button"
              className="danger"
              autoFocus
              onClick={() => void purge(null)}
              disabled={purging}
            >
              {purging ? "Deleting..." : "Delete everything"}
            </button>
            <button
              type="button"
              onClick={() => setConfirming(null)}
              disabled={purging}
            >
              Cancel
            </button>
          </div>
        ) : (
          <button
            type="button"
            className="danger"
            onClick={() => {
              setConfirming({ workspace: null });
              setStatus("");
            }}
            disabled={purging || !hasHistory}
          >
            Delete all history
          </button>
        )}
      </div>
    </div>
  );
}
