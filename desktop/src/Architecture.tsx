import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowClockwiseIcon,
  CaretDownIcon,
  CheckIcon,
  EyeIcon,
  SparkleIcon,
  TreeStructureIcon,
  WarningCircleIcon,
} from "@phosphor-icons/react";
import type { LayoutedLikeC4ModelData } from "@likec4/core";
import { LikeC4Model } from "@likec4/core/model";
import { LikeC4ModelProvider, LikeC4View } from "@likec4/diagram";
import {
  diagnosticLocation,
  isCurrentLoad,
  pickModelRoot,
  pickViewId,
  rootLabel,
  type ArchitectureDiagnostic,
  type ArchitectureDiscovery,
  type ArchitectureLoad,
} from "./architecture-model";
import { buildArchitecturePrompt } from "./architecture-prompt";

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}

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

function Dropdown({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: { value: string; label: string; title?: string }[];
  onChange: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemsRef = useRef<Array<HTMLButtonElement | null>>([]);
  const selected =
    options.find((option) => option.value === value) ?? options[0];

  useEffect(() => {
    if (!open) return;
    itemsRef.current[active]?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: PointerEvent) {
      if (!wrapRef.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  function show() {
    const index = options.findIndex((option) => option.value === value);
    setActive(index < 0 ? 0 : index);
    setOpen(true);
  }

  function close(refocus = false) {
    setOpen(false);
    if (refocus) triggerRef.current?.focus();
  }

  function focusItem(index: number) {
    const next = Math.max(0, Math.min(options.length - 1, index));
    setActive(next);
    itemsRef.current[next]?.focus();
  }

  function choose(option: { value: string }) {
    onChange(option.value);
    close(true);
  }

  function onTriggerKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (open || !["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
      return;
    }
    event.preventDefault();
    show();
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      close(true);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      focusItem(active + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusItem(active - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusItem(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusItem(options.length - 1);
    } else if (event.key === "Tab") {
      close(true);
    }
  }

  return (
    <div className="arch-dropdown" ref={wrapRef}>
      <button
        ref={triggerRef}
        type="button"
        className="arch-select"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        onClick={() => (open ? close() : show())}
        onKeyDown={onTriggerKeyDown}
      >
        <span className="arch-select-label">{selected?.label ?? value}</span>
        <CaretDownIcon size={10} className="arch-select-caret" />
      </button>
      {open && (
        <div
          className="arch-menu"
          role="menu"
          aria-label={label}
          onKeyDown={onMenuKeyDown}
        >
          {options.map((option, index) => (
            <button
              key={option.value}
              ref={(element) => {
                itemsRef.current[index] = element;
              }}
              type="button"
              role="menuitemradio"
              aria-checked={option.value === value}
              className="arch-menu-item"
              title={option.title}
              onClick={() => choose(option)}
            >
              <span>{option.label}</span>
              {option.value === value && <CheckIcon size={11} weight="bold" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

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

  const currentLoad =
    load && selectedRoot !== null && load.root === selectedRoot ? load : null;
  const selectedModel =
    discovery?.models.find((model) => model.root === selectedRoot) ?? null;
  const runtimeReady = discovery?.runtime.ready ?? false;

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

  function refresh() {
    setReload((value) => value + 1);
    onRefresh();
  }

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

  const busy = discoveryLoading || loading;
  const viewSelectable = (currentLoad?.views.length ?? 0) > 1;

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
          <Dropdown
            label="Architecture model"
            value={selectedRoot ?? ""}
            options={discovery.models.map((model) => ({
              value: model.root,
              label: rootLabel(model.root),
              title: model.root,
            }))}
            onChange={(root) => {
              selectedRootRef.current = root;
              setSelectedRoot(root);
            }}
          />
        )}

        {viewSelectable && currentLoad && (
          <Dropdown
            label="Diagram view"
            value={selectedViewId ?? ""}
            options={currentLoad.views.map((view) => ({
              value: view.id,
              label: view.title || view.id,
              title: view.title || view.id,
            }))}
            onChange={(viewId) => {
              selectedViewRef.current = viewId;
              setSelectedViewId(viewId);
            }}
          />
        )}

        <div className="arch-actions">
          <button
            type="button"
            className={`ghost icon-button arch-ignored${
              includeIgnored ? " active" : ""
            }`}
            onClick={() => setIncludeIgnored((value) => !value)}
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

      {discovery && !runtimeReady && (
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

      {!discovery ? (
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
      ) : discovery.models.length === 0 ? (
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
            <button type="button" className="ghost" onClick={() => setIncludeIgnored((value) => !value)}>
              {includeIgnored ? "Hide ignored files" : "Scan ignored files"}
            </button>
          </div>
          <p className="arch-small">
            Oaklet only reads existing model files. Nothing is generated until
            you send this request in Chat.
          </p>
        </div>
      ) : !runtimeReady ? (
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
      ) : !currentLoad ? (
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
      ) : !currentLoad.model ? (
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
      ) : currentLoad.views.length === 0 || !selectedViewId ? (
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
      ) : modelState.error || !renderedModel ? (
        <div className="arch-empty">
          <WarningCircleIcon size={26} />
          <h2>Could not render the layout</h2>
          <p>{modelState.error || "LikeC4 returned an empty model."}</p>
          <button type="button" onClick={refresh}>
            Refresh
          </button>
        </div>
      ) : (
        <div className="arch-canvas">
          <LikeC4ModelProvider likec4model={renderedModel}>
            <LikeC4View
              key={`${currentLoad.sourceDigest}:${selectedViewId}`}
              viewId={selectedViewId}
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
            />
          </LikeC4ModelProvider>
        </div>
      )}
    </section>
  );
}
