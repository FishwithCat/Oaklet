import type { ReactNode } from "react";
import {
  ArrowClockwiseIcon,
  SparkleIcon,
  TreeStructureIcon,
  WarningCircleIcon,
} from "@phosphor-icons/react";
import {
  diagnosticLocation,
  type ArchitectureDiagnostic,
  type ArchitectureDiscovery,
  type ArchitectureLoad,
  type ArchitectureModelRoot,
} from "./architecture-model";

function Diagnostics({ items }: { items: ArchitectureDiagnostic[] }) {
  if (items.length === 0) return null;
  return (
    <ul className="arch-diagnostics">
      {items.map((diagnostic, index) => (
        <li key={`${diagnostic.file ?? "model"}-${diagnostic.line ?? 0}-${index}`}>
          <span className="arch-diagnostic-loc">
            {diagnosticLocation(diagnostic)}
          </span>
          <span>{diagnostic.message}</span>
        </li>
      ))}
    </ul>
  );
}

export function ArchitectureBanners({
  discovery,
  discoveryError,
  selectedModel,
  currentLoad,
  loadError,
  refresh,
}: {
  discovery: ArchitectureDiscovery | null;
  discoveryError: string;
  selectedModel: ArchitectureModelRoot | null;
  currentLoad: ArchitectureLoad | null;
  loadError: string;
  refresh: () => void;
}) {
  return (
    <>
      {discoveryError && (
        <div className="arch-banner error" role="alert">
          <WarningCircleIcon size={15} />
          <div>
            <strong>Could not scan this workspace.</strong>
            <p>{discoveryError}</p>
            <button type="button" className="ghost" onClick={refresh}>
              Try again
            </button>
          </div>
        </div>
      )}

      {discovery && !discovery.runtime.ready && (
        <div className="arch-banner warning">
          <WarningCircleIcon size={15} />
          <div>
            <strong>LikeC4 preview needs a runtime.</strong>
            <p>{discovery.runtime.message}</p>
          </div>
        </div>
      )}

      {selectedModel?.unsupportedConfig && (
        <div className="arch-banner warning">
          <WarningCircleIcon size={15} />
          <div>
            <strong>Executable LikeC4 configuration is not run.</strong>
            <p>
              <code>{selectedModel.unsupportedConfig}</code> may affect how this
              model is interpreted. Oaklet previews data-only JSON configuration
              and source files; commit a supported configuration or adapt the
              model for preview.
            </p>
          </div>
        </div>
      )}

      {currentLoad?.stale && (
        <div className="arch-banner warning">
          <WarningCircleIcon size={15} />
          <div>
            <strong>Showing the last valid diagram.</strong>
            <p>
              The current sources did not compile, so the previous layout was
              kept. Fix the model, then refresh.
            </p>
            <Diagnostics items={currentLoad.diagnostics} />
          </div>
        </div>
      )}

      {currentLoad?.error && !currentLoad.stale && (
        <div className="arch-banner error" role="alert">
          <WarningCircleIcon size={15} />
          <div>
            <strong>{currentLoad.error}</strong>
            <Diagnostics items={currentLoad.diagnostics} />
          </div>
        </div>
      )}

      {currentLoad &&
        !currentLoad.stale &&
        !currentLoad.error &&
        currentLoad.diagnostics.length > 0 && (
          <div className="arch-banner warning">
            <WarningCircleIcon size={15} />
            <div>
              <strong>Model diagnostics</strong>
              <Diagnostics items={currentLoad.diagnostics} />
            </div>
          </div>
        )}

      {loadError && (
        <div className="arch-banner error" role="alert">
          <WarningCircleIcon size={15} />
          <div>
            <strong>Could not load the architecture model.</strong>
            <p>{loadError}</p>
            <button type="button" className="ghost" onClick={refresh}>
              Try again
            </button>
          </div>
        </div>
      )}

      {discovery && discovery.warnings.length > 0 && (
        <details className="arch-banner muted">
          <summary>
            {discovery.warnings.length} note
            {discovery.warnings.length === 1 ? "" : "s"} about this scan
          </summary>
          <ul>
            {discovery.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        </details>
      )}
    </>
  );
}

/**
 * Renders the loading, warning, and empty states for the architecture view.
 * When every precondition holds, the caller's diagram is rendered instead.
 */
export function ArchitectureStatus({
  children,
  discovery,
  discoveryError,
  runtimeReady,
  currentLoad,
  loadError,
  selectedViewId,
  modelError,
  hasModel,
  running,
  includeIgnored,
  refresh,
  generate,
  update,
  toggleIgnored,
}: {
  children: ReactNode;
  discovery: ArchitectureDiscovery | null;
  discoveryError: string;
  runtimeReady: boolean;
  currentLoad: ArchitectureLoad | null;
  loadError: string;
  selectedViewId: string | null;
  modelError: string;
  hasModel: boolean;
  running: boolean;
  includeIgnored: boolean;
  refresh: () => void;
  generate: () => void;
  update: () => void;
  toggleIgnored: () => void;
}) {
  if (!discovery) {
    return (
      <div className="arch-empty">
        {discoveryError ? (
          <>
            <WarningCircleIcon size={26} />
            <h2>Architecture is unavailable</h2>
            <p>{discoveryError}</p>
            <button type="button" onClick={refresh}>
              Try again
            </button>
          </>
        ) : (
          <>
            <ArrowClockwiseIcon size={24} className="spin" />
            <p>Scanning for LikeC4 models...</p>
          </>
        )}
      </div>
    );
  }

  if (discovery.models.length === 0) {
    return (
      <div className="arch-empty">
        <TreeStructureIcon size={28} />
        <h2>No architecture model yet</h2>
        <p>
          Ask the configured agent to inspect this workspace and create{" "}
          <code>{discovery.defaultPath}</code>. Generation reads project files
          with the configured provider, like any coding request.
        </p>
        <div className="arch-empty-actions">
          <button
            type="button"
            className="primary arch-generate"
            onClick={generate}
            disabled={running}
            title={
              running
                ? "Wait for the current agent run to finish"
                : "Ask the agent to inspect the workspace and create the model"
            }
          >
            <SparkleIcon size={14} />
            Generate architecture
          </button>
          <button type="button" className="ghost" onClick={toggleIgnored}>
            {includeIgnored ? "Hide ignored files" : "Scan ignored files"}
          </button>
        </div>
        <p className="arch-small">
          Oaklet only reads existing model files. Nothing is generated until
          you send this request in Chat.
        </p>
      </div>
    );
  }

  if (!runtimeReady) {
    return (
      <div className="arch-empty">
        <WarningCircleIcon size={26} />
        <h2>LikeC4 preview is unavailable</h2>
        <p>{discovery.runtime.message}</p>
        <p className="arch-small">
          Install Node.js 22.22.3 or newer, or point OAKLET_NODE at it, then
          refresh.
        </p>
        <button type="button" onClick={refresh}>
          Refresh
        </button>
      </div>
    );
  }

  if (!currentLoad) {
    return (
      <div className="arch-empty">
        {loadError ? (
          <>
            <WarningCircleIcon size={26} />
            <h2>Could not load the model</h2>
            <p>{loadError}</p>
            <button type="button" onClick={refresh}>
              Try again
            </button>
          </>
        ) : (
          <>
            <ArrowClockwiseIcon size={24} className="spin" />
            <p>Loading the architecture model...</p>
          </>
        )}
      </div>
    );
  }

  if (!currentLoad.model) {
    return (
      <div className="arch-empty">
        <WarningCircleIcon size={26} />
        <h2>The model could not be rendered</h2>
        <p>
          {currentLoad.error ??
            "LikeC4 did not return a layouted model for these sources."}
        </p>
        <Diagnostics items={currentLoad.diagnostics} />
        <button
          type="button"
          className="primary"
          onClick={update}
          disabled={running}
        >
          <SparkleIcon size={14} />
          Update with agent
        </button>
      </div>
    );
  }

  if (currentLoad.views.length === 0 || !selectedViewId) {
    return (
      <div className="arch-empty">
        <TreeStructureIcon size={26} />
        <h2>This model has no views</h2>
        <p>
          Add a view to the LikeC4 model, or ask the agent to add context and
          container views.
        </p>
        <button
          type="button"
          className="primary"
          onClick={update}
          disabled={running}
        >
          <SparkleIcon size={14} />
          Update with agent
        </button>
      </div>
    );
  }

  if (modelError || !hasModel) {
    return (
      <div className="arch-empty">
        <WarningCircleIcon size={26} />
        <h2>Could not render the layout</h2>
        <p>{modelError || "LikeC4 returned an empty model."}</p>
        <button type="button" onClick={refresh}>
          Refresh
        </button>
      </div>
    );
  }

  return <>{children}</>;
}
