import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { detectPlatform } from "../../app/shortcuts";
import { errorText } from "../../shared/error-text";

type UpdateInfo = {
  currentVersion: string;
  version: string;
  notes: string | null;
  releaseUrl: string;
};

type Status =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "installing"
  | "installed";

export function UpdatesSettings() {
  const [currentVersion, setCurrentVersion] = useState("");
  const [status, setStatus] = useState<Status>("idle");
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [error, setError] = useState("");
  const [confirming, setConfirming] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    invoke<string>("app_version")
      .then((version) => {
        if (mounted.current) setCurrentVersion(version);
      })
      .catch(() => {});
    return () => {
      mounted.current = false;
    };
  }, []);

  async function check() {
    setStatus("checking");
    setUpdate(null);
    setError("");
    setConfirming(false);
    try {
      const found = await invoke<UpdateInfo | null>("check_for_update");
      if (!mounted.current) return;
      setUpdate(found);
      setCurrentVersion(found?.currentVersion ?? currentVersion);
      setStatus(found ? "available" : "current");
    } catch (err) {
      if (!mounted.current) return;
      setError(errorText(err));
      setStatus("idle");
    }
  }

  async function install() {
    setStatus("installing");
    setError("");
    try {
      const version = await invoke<string>("install_update");
      if (!mounted.current) return;
      setStatus("installed");
      setCurrentVersion(version);
      setConfirming(false);
    } catch (err) {
      if (!mounted.current) return;
      setError(errorText(err));
      setStatus("available");
    }
  }

  const platform = detectPlatform();
  const busy = status === "checking" || status === "installing";

  return (
    <div className="updates-panel">
      <p className="field-hint updates-intro">
        Oaklet checks for updates only when you ask. It does not contact the
        update server on startup, when a window gains focus, or on a timer.
        Downloads are verified with the update signing key before they are
        installed.
      </p>

      <div className="update-card">
        <div>
          <strong>Current version</strong>
          <p className="field-hint">{currentVersion || "Reading version..."}</p>
        </div>
        <button type="button" onClick={check} disabled={busy}>
          {status === "checking" ? "Checking..." : "Check for updates"}
        </button>
      </div>

      {status === "current" && (
        <p className="update-message ok" role="status">
          Oaklet is up to date.
        </p>
      )}

      {update && status !== "installed" && (
        <div className="update-card details" role="status">
          <div>
            <strong>Version {update.version} is available</strong>
            <p className="field-hint">
              You are running {update.currentVersion}. The update package is
              signed for this exact version.
            </p>
            {update.notes && (
              <p className="update-notes">{update.notes}</p>
            )}
          </div>
          <div className="update-actions">
            <button
              type="button"
              onClick={() => void openUrl(update.releaseUrl).catch(() => {})}
              disabled={busy}
            >
              Open release page
            </button>
            <button
              type="button"
              className="primary"
              onClick={() => setConfirming(true)}
              disabled={busy}
            >
              Download and install
            </button>
          </div>
        </div>
      )}

      {status === "installed" && (
        <div className="update-card details" role="status">
          <div>
            <strong>Version {currentVersion} is installed</strong>
            <p className="field-hint">
              Quit Oaklet and open it again to run the new version. Your
              sessions, workspace, and settings stay where they are.
            </p>
          </div>
        </div>
      )}

      {confirming && update && status !== "installed" && (
        <div className="update-card details" role="alertdialog" aria-live="assertive">
          <div>
            <strong>Install version {update.version} now?</strong>
            <p className="field-hint">
              {platform === "windows"
                ? "Windows installs the update after Oaklet closes. Finish or stop any running task first, then reopen Oaklet when the installer finishes."
                : "The update is installed in place. Oaklet will not restart itself; quit and reopen it to use the new version."}
            </p>
          </div>
          <div className="update-actions">
            <button
              type="button"
              onClick={() => setConfirming(false)}
              disabled={busy}
            >
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              onClick={install}
              disabled={busy}
            >
              {status === "installing" ? "Installing..." : "Install update"}
            </button>
          </div>
        </div>
      )}

      {error && (
        <p className="update-message error" role="alert">
          {error}. Oaklet is still running {currentVersion}.
        </p>
      )}
    </div>
  );
}
