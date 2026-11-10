import { useMemo } from "react";
import {
  ArrowClockwiseIcon,
  EyeIcon,
  SparkleIcon,
  TreeStructureIcon,
} from "@phosphor-icons/react";
import type { LayoutedLikeC4ModelData } from "@likec4/core";
import { LikeC4Model } from "@likec4/core/model";
import { LikeC4ModelProvider, LikeC4View } from "@likec4/diagram";
import { errorText } from "../../shared/error-text";
import { ArchitectureDropdown } from "./ArchitectureDropdown";
import {
  ArchitectureBanners,
  ArchitectureStatus,
} from "./ArchitectureStatus";
import { buildArchitecturePrompt } from "./architecture-prompt";
import { rootLabel } from "./architecture-model";
import { useArchitectureModel } from "./useArchitectureModel";

export default function Architecture({
  active,
  workspace,
  refreshToken,
  running,
  onPrompt,
  onRefresh,
}: {
  active: boolean;
  workspace: string;
  refreshToken: number;
  running: boolean;
  onPrompt: (prompt: string) => void;
  onRefresh: () => void;
}) {
  const {
    discovery,
    discoveryError,
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
    refresh,
    busy,
    viewSelectable,
  } = useArchitectureModel({ active, workspace, refreshToken, onRefresh });

  const modelState = useMemo(() => {
    if (!currentLoad?.model) return { model: null, error: "" };
    try {
      return {
        model: LikeC4Model.create(
          currentLoad.model as LayoutedLikeC4ModelData,
        ),
        error: "",
      };
    } catch (error) {
      return { model: null, error: errorText(error) };
    }
  }, [currentLoad?.model]);
  const renderedModel = modelState.model;

  function generate() {
    if (running || !discovery) return;
    onPrompt(
      buildArchitecturePrompt({
        mode: "create",
        modelRoot: selectedRoot ?? ".",
        modelPaths: [],
        defaultPath: discovery.defaultPath,
      }),
    );
  }

  function update() {
    if (running || !discovery || !selectedRoot) return;
    const targets = currentLoad?.sources ?? selectedModel?.sources ?? [];
    if (targets.length === 0) return;
    onPrompt(
      buildArchitecturePrompt({
        mode: "update",
        modelRoot: selectedRoot,
        modelPaths: targets.map((source) =>
          typeof source === "string" ? source : source.path,
        ),
        defaultPath: discovery.defaultPath,
        diagnostics: currentLoad?.diagnostics ?? [],
      }),
    );
  }

  return (
    <section
      className="arch"
      hidden={!active}
      aria-label="Architecture"
      aria-busy={busy}
    >
      <header className="arch-head">
        <span className="arch-icon" aria-hidden="true">
          <TreeStructureIcon size={14} />
        </span>
        <span className="arch-title">Architecture</span>

        {discovery && discovery.models.length > 1 && (
          <ArchitectureDropdown
            label="Architecture model"
            value={selectedRoot ?? ""}
            options={discovery.models.map((model) => ({
              value: model.root,
              label: rootLabel(model.root),
              title: model.root,
            }))}
            onChange={selectRoot}
          />
        )}

        {viewSelectable && currentLoad && (
          <ArchitectureDropdown
            label="Diagram view"
            value={selectedViewId ?? ""}
            options={currentLoad.views.map((view) => ({
              value: view.id,
              label: view.title || view.id,
              title: view.title || view.id,
            }))}
            onChange={selectView}
          />
        )}

        <div className="arch-actions">
          <button
            type="button"
            className={`ghost icon-button arch-ignored${
              includeIgnored ? " active" : ""
            }`}
            onClick={toggleIgnored}
            aria-label={
              includeIgnored ? "Hide ignored files" : "Scan ignored files"
            }
            aria-pressed={includeIgnored}
            title={
              includeIgnored
                ? "Hide files normally ignored by git"
                : "Also scan files ignored by git"
            }
          >
            <EyeIcon size={14} weight={includeIgnored ? "fill" : "regular"} />
          </button>
          <button
            type="button"
            className="ghost icon-button arch-refresh"
            onClick={refresh}
            disabled={busy}
            aria-label="Refresh architecture"
            title="Refresh architecture"
          >
            <ArrowClockwiseIcon size={14} className={busy ? "spin" : ""} />
          </button>
          <button
            type="button"
            className="arch-agent"
            onClick={update}
            disabled={running || (!currentLoad && !selectedModel)}
            title={
              running
                ? "Wait for the current agent run to finish"
                : "Ask the agent to update the selected model"
            }
          >
            <SparkleIcon size={13} />
            Update with agent
          </button>
        </div>
      </header>

      <ArchitectureBanners
        discovery={discovery}
        discoveryError={discoveryError}
        selectedModel={selectedModel}
        currentLoad={currentLoad}
        loadError={loadError}
        refresh={refresh}
      />

      <ArchitectureStatus
        discovery={discovery}
        discoveryError={discoveryError}
        runtimeReady={runtimeReady}
        currentLoad={currentLoad}
        loadError={loadError}
        selectedViewId={selectedViewId}
        modelError={modelState.error}
        hasModel={renderedModel !== null}
        running={running}
        includeIgnored={includeIgnored}
        refresh={refresh}
        generate={generate}
        update={update}
        toggleIgnored={toggleIgnored}
      >
        <div className="arch-canvas">
          <LikeC4ModelProvider likec4model={renderedModel!}>
            <LikeC4View
              key={`${currentLoad?.sourceDigest}:${selectedViewId}`}
              viewId={selectedViewId ?? ""}
              layoutType="auto"
              colorScheme="light"
              background="dots"
              pannable
              zoomable
              controls
              fitView
              keepAspectRatio={false}
              injectFontCss={false}
              browser={false}
              enableElementDetails
              enableRelationshipDetails
              enableNotes
              className="arch-diagram"
            >
              {/* shortcut: LikeC4 1.60 needs styles inside its shadow root; recheck logo/breadcrumb slots on upgrade. */}
              <style>{`
                [data-panel="navigation"] > div:nth-child(-n + 2) {
                  display: none;
                }
              `}</style>
            </LikeC4View>
          </LikeC4ModelProvider>
        </div>
      </ArchitectureStatus>
    </section>
  );
}
