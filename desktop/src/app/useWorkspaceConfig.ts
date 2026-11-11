import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { message, open } from "@tauri-apps/plugin-dialog";
import { errorText } from "../shared/error-text";
import type { AppConfig } from "../features/settings/config";

/**
 * Load the stored config and handle the workspace picker. Workspace refresh and
 * command-allowance refresh stay with App, which coordinates them on success.
 */
export function useWorkspaceConfig({
  beforeWorkspaceChange,
  onFirstRun,
  onWorkspaceChanged,
}: {
  beforeWorkspaceChange: () => Promise<boolean>;
  onFirstRun: () => void;
  onWorkspaceChanged: () => void;
}) {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [browsingWorkspace, setBrowsingWorkspace] = useState(false);
  const onFirstRunRef = useRef(onFirstRun);
  const onWorkspaceChangedRef = useRef(onWorkspaceChanged);

  useEffect(() => {
    onFirstRunRef.current = onFirstRun;
    onWorkspaceChangedRef.current = onWorkspaceChanged;
  });

  useEffect(() => {
    invoke<AppConfig>("load_config")
      .then((loaded) => {
        setConfig(loaded);
        if (!loaded.apiKey) onFirstRunRef.current();
      })
      .catch(() => setLoadFailed(true));
  }, []);

  async function chooseWorkspace() {
    if (!config || browsingWorkspace) return;
    setBrowsingWorkspace(true);
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: config.workspace || undefined,
      });
      if (typeof picked !== "string" || picked === config.workspace) return;
      if (!await beforeWorkspaceChange()) return;
      const saved = await invoke<AppConfig>("save_config", {
        config: { ...config, workspace: picked },
      });
      setConfig(saved);
      onWorkspaceChangedRef.current();
    } catch (error) {
      await message(`Could not switch workspace: ${errorText(error)}`, {
        title: "Oaklet",
        kind: "error",
      });
    } finally {
      setBrowsingWorkspace(false);
    }
  }

  return {
    config,
    loadFailed,
    browsingWorkspace,
    chooseWorkspace,
    setConfig,
  };
}
