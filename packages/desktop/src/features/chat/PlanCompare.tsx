import { useEffect, useMemo, useRef, useState } from "react";
import type { LayoutedLikeC4ModelData } from "@likec4/core";
import { filterCompareView } from "@likec4/core/compute-view";
import { LikeC4Model } from "@likec4/core/model";
import type { LayoutedView } from "@likec4/core/types";
import { LikeC4ModelProvider, LikeC4View } from "@likec4/diagram";
import { errorText } from "../../shared/error-text";
import { useResolvedTheme } from "../settings/theme";
import {
  COMPARE_MODES,
  planViews,
  type CompareMode,
  type PlanCompareData,
  type PlanItem,
} from "./plan-model";

type Load =
  | { status: "loading" }
  | { status: "ready"; data: PlanCompareData }
  | { status: "error"; message: string };

type Prepared =
  | { kind: "ready"; model: ReturnType<typeof LikeC4Model.create>; viewId: string }
  | { kind: "error"; error: string };

export function PlanCompare({
  plan,
  onLoad,
}: {
  plan: PlanItem;
  onLoad: (version: number, viewId: string) => Promise<PlanCompareData>;
}) {
  const views = plan.validation ? planViews(plan.validation) : [];
  const [selected, setSelected] = useState(views[0]?.id ?? "");
  const [mode, setMode] = useState<CompareMode>("changes");
  const [loads, setLoads] = useState<Record<string, Load>>({});
  const [attempt, setAttempt] = useState(0);
  const requested = useRef(new Set<string>());
  const loadRef = useRef(onLoad);
  const theme = useResolvedTheme();
  const version = plan.version;

  useEffect(() => {
    loadRef.current = onLoad;
  }, [onLoad]);

  useEffect(() => {
    setSelected(views[0]?.id ?? "");
    setMode("changes");
    setLoads({});
    requested.current.clear();
  }, [version]);

  const key = `${version}:${selected}`;
  useEffect(() => {
    if (!selected || requested.current.has(key)) return;
    requested.current.add(key);
    let cancelled = false;
    setLoads((current) => ({ ...current, [key]: { status: "loading" } }));
    loadRef.current(version, selected)
      .then((data) => {
        if (cancelled) return;
        if (
          data.version !== version ||
          data.viewId !== selected ||
          data.sourceDigest !== plan.validation?.sourceDigest
        ) {
          throw new Error(
            "The architecture preview belongs to a different plan version.",
          );
        }
        setLoads((current) => ({
          ...current,
          [key]: { status: "ready", data },
        }));
      })
      .catch((error) => {
        if (cancelled) return;
        setLoads((current) => ({
          ...current,
          [key]: { status: "error", message: errorText(error) },
        }));
      });
    return () => {
      cancelled = true;
      requested.current.delete(key);
    };
  }, [attempt, key, plan.validation?.sourceDigest, selected, version]);

  const load = loads[key];

  const prepared = useMemo<Prepared | null>(() => {
    if (load?.status !== "ready") return null;
    try {
      const view = filterCompareView(
        load.data.view as LayoutedView,
        mode,
      );
      const model = LikeC4Model.create({
        ...(load.data.model as LayoutedLikeC4ModelData),
        views: { [load.data.viewId]: view },
      });
      return { kind: "ready", model, viewId: load.data.viewId };
    } catch (error) {
      return { kind: "error", error: errorText(error) };
    }
  }, [load, mode]);

  function retry() {
    requested.current.delete(key);
    setAttempt((value) => value + 1);
  }

  if (views.length === 0) {
    return (
      <p className="plan-compare-empty">
        LikeC4 did not report an affected view for this diff.
      </p>
    );
  }

  return (
    <section className="plan-compare" aria-label="Architecture comparison">
      <div className="plan-compare-head">
        <span className="plan-label">Architecture comparison</span>
        {views.length > 1 && (
          <select
            className="plan-view-select"
            value={selected}
            onChange={(event) => {
              setSelected(event.target.value);
              setMode("changes");
            }}
            aria-label="Affected view"
          >
            {views.map((view) => (
              <option key={view.id} value={view.id}>
                {view.title || view.id}
              </option>
            ))}
          </select>
        )}
      </div>

      <div className="plan-compare-tabs" role="tablist" aria-label="Model state">
        {COMPARE_MODES.map((item) => (
          <button
            type="button"
            key={item.id}
            role="tab"
            aria-selected={mode === item.id}
            className={mode === item.id ? "active" : ""}
            onClick={() => setMode(item.id)}
          >
            {item.label}
          </button>
        ))}
      </div>

      <div className="plan-compare-legend" aria-label="Change legend">
        <span className="added">+ Added</span>
        <span className="modified">~ Modified</span>
        <span className="removed">− Removed</span>
      </div>

      {load?.status === "loading" && (
        <p className="plan-compare-status" role="status">
          Laying out the comparison…
        </p>
      )}
      {load?.status === "error" && (
        <div className="plan-compare-error">
          <p>{load.message}</p>
          <button type="button" className="ghost" onClick={retry}>
            Retry preview
          </button>
        </div>
      )}
      {prepared?.kind === "error" && (
        <p className="plan-compare-error">{prepared.error}</p>
      )}
      {prepared?.kind === "ready" && (
        <div className="plan-compare-canvas">
          <LikeC4ModelProvider likec4model={prepared.model}>
            <LikeC4View
              key={`${version}:${prepared.viewId}:${mode}`}
              viewId={prepared.viewId}
              layoutType="auto"
              colorScheme={theme}
              background="dots"
              pannable
              zoomable
              controls
              fitView
              keepAspectRatio={false}
              injectFontCss={false}
              browser={false}
              enableElementDetails={false}
              enableRelationshipDetails={false}
              enableNotes={false}
              className="plan-compare-diagram"
            >
              <style>{`
                .react-flow__node.likec4-diff-added::after,
                .react-flow__node.likec4-diff-modified::after,
                .react-flow__node.likec4-diff-removed::after {
                  position: absolute;
                  top: -9px;
                  left: 8px;
                  padding: 1px 5px;
                  border-radius: 999px;
                  color: #fff;
                  font: 600 9px/1.4 var(--fonts-body, sans-serif);
                  letter-spacing: 0.2px;
                  pointer-events: none;
                }
                .react-flow__node.likec4-diff-added::after {
                  content: "+ Added";
                  background: #2f9e44;
                }
                .react-flow__node.likec4-diff-modified::after {
                  content: "~ Modified";
                  background: #b7791f;
                }
                .react-flow__node.likec4-diff-removed::after {
                  content: "− Removed";
                  background: #c92a2a;
                }
              `}</style>
            </LikeC4View>
          </LikeC4ModelProvider>
        </div>
      )}

    </section>
  );
}
