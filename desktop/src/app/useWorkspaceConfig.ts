import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { message, open } from "@tauri-apps/plugin-dialog";
import { errorText } from "../shared/error-text";
import type { AppConfig } from "../features/settings/config";

/**
 * Load global settings and open a workspace picker. A workspace opens in its
 * own window; this window never changes projects underneath its chat tabs.
 */
export function useWorkspaceConfig({
  onFirstRun,
}: {
  onFirstRun: () => void;
}) {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [browsingWorkspace, setBrowsingWorkspace] = useState(false);
  const onFirstRunRef = useRef(onFirstRun);

  useEffect(() => {
    onFirstRunRef.current = onFirstRun;
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
      if (typeof picked !== "string") return;
      await invoke("open_workspace", { workspace: picked });
    } catch (error) {
      await message(`Could not open workspace: ${errorText(error)}`, {
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
