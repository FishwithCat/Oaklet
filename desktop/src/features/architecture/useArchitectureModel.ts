import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorText } from "../../shared/error-text";
import {
  isCurrentLoad,
  pickModelRoot,
  pickViewId,
  type ArchitectureDiscovery,
  type ArchitectureLoad,
} from "./architecture-model";

export function useArchitectureModel({
  active,
  workspace,
  refreshToken,
  onRefresh,
}: {
  active: boolean;
  workspace: string;
  refreshToken: number;
  onRefresh: () => void;
}) {
  const [discovery, setDiscovery] = useState<ArchitectureDiscovery | null>(null);
  const [discoveryError, setDiscoveryError] = useState("");
  const [discoveryLoading, setDiscoveryLoading] = useState(false);
  const [selectedRoot, setSelectedRoot] = useState<string | null>(null);
  const [includeIgnored, setIncludeIgnored] = useState(false);
  const [epoch, setEpoch] = useState(0);
  const [load, setLoad] = useState<ArchitectureLoad | null>(null);
  const [loadError, setLoadError] = useState("");
  const [loading, setLoading] = useState(false);
  const [selectedViewId, setSelectedViewId] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  const selectedRootRef = useRef<string | null>(null);
  const selectedViewRef = useRef<string | null>(null);
  const loadSequenceRef = useRef(0);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;

    setDiscoveryLoading(true);
    setDiscoveryError("");
    invoke<ArchitectureDiscovery>("architecture_discover", { includeIgnored })
      .then((next) => {
        if (cancelled || next.workspace !== workspace) return;
        setDiscovery(next);
        const root = pickModelRoot(next.models, selectedRootRef.current);
        selectedRootRef.current = root;
        setSelectedRoot(root);
        setEpoch((value) => value + 1);
      })
      .catch((error) => {
        if (!cancelled) setDiscoveryError(errorText(error));
      })
      .finally(() => {
        if (!cancelled) setDiscoveryLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [active, workspace, refreshToken, reload, includeIgnored]);

  useEffect(() => {
    if (!active) return;
    if (!selectedRoot) {
      setLoad(null);
      setLoadError("");
      setLoading(false);
      selectedViewRef.current = null;
      setSelectedViewId(null);
      return;
    }
    if (!discovery || discovery.workspace !== workspace) return;
    if (!discovery.runtime.ready) {
      setLoad(null);
      setLoadError("");
      setLoading(false);
      return;
    }

    const sequence = loadSequenceRef.current + 1;
    loadSequenceRef.current = sequence;
    let cancelled = false;

    setLoading(true);
    setLoadError("");
    setLoad((previous) =>
      previous && previous.root === selectedRoot ? previous : null,
    );
    invoke<ArchitectureLoad>("architecture_load", {
      root: selectedRoot,
      includeIgnored,
    })
      .then((next) => {
        if (
          cancelled ||
          !isCurrentLoad(
            sequence,
            loadSequenceRef.current,
            next.workspace,
            workspace,
          )
        ) {
          return;
        }
        setLoad(next);
        const view = pickViewId(next.views, selectedViewRef.current);
        selectedViewRef.current = view;
        setSelectedViewId(view);
      })
      .catch((error) => {
        if (cancelled || sequence !== loadSequenceRef.current) return;
        setLoadError(errorText(error));
      })
      .finally(() => {
        if (!cancelled && sequence === loadSequenceRef.current) {
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [active, workspace, selectedRoot, epoch]);

  function selectRoot(root: string) {
    selectedRootRef.current = root;
    setSelectedRoot(root);
  }

  function selectView(viewId: string) {
    selectedViewRef.current = viewId;
    setSelectedViewId(viewId);
  }

  function toggleIgnored() {
    setIncludeIgnored((value) => !value);
  }

  function refresh() {
    setReload((value) => value + 1);
    onRefresh();
  }

  const currentLoad =
    load && selectedRoot !== null && load.root === selectedRoot ? load : null;
  const selectedModel =
    discovery?.models.find((model) => model.root === selectedRoot) ?? null;
  const runtimeReady = discovery?.runtime.ready ?? false;

  return {
    discovery,
    discoveryError,
    discoveryLoading,
    includeIgnored,
    selectedRoot,
    selectRoot,
    selectView,
    selectedViewId,
    toggleIgnored,
    currentLoad,
    selectedModel,
    runtimeReady,
    loadError,
    loading,
    refresh,
    busy: discoveryLoading || loading,
    viewSelectable: (currentLoad?.views.length ?? 0) > 1,
  };
}
