import { useState } from "react";
import { MarkdownText } from "../../shared/Markdown";
import {
  canConfirmPlan,
  confirmLabel,
  planBadge,
  planNotes,
  planSummary,
  planViews,
  type PlanItem,
} from "./plan-model";

export function PlanCard({
  plan,
  latestVersion,
  running,
  onConfirm,
  onRevise,
  onOpenFile,
}: {
  plan: PlanItem;
  latestVersion: number | null;
  running: boolean;
  onConfirm: (version: number) => Promise<boolean>;
  onRevise: () => void;
  onOpenFile: (href: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [copied, setCopied] = useState(false);
  const validation = plan.validation ?? null;
  const badge = planBadge(plan, latestVersion);
  const notes = planNotes(plan);
  const views = validation ? planViews(validation) : [];
  const summary = validation ? planSummary(validation) : "";
  const confirmable = canConfirmPlan(plan, latestVersion, running) && !confirming;

  async function confirm() {
    setConfirming(true);
    try {
      await onConfirm(plan.version);
    } finally {
      setConfirming(false);
    }
  }

  function copyDsl() {
    if (!validation?.dsl) return;
    void navigator.clipboard
      ?.writeText(validation.dsl)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => setCopied(false));
  }

  return (
    <section
      className={`plan-card${plan.status === "accepted" ? " accepted" : ""}`}
      data-seq={plan.seq}
      aria-label={`Plan version ${plan.version}`}
    >
      <header className="plan-head">
        <span className="plan-title">Plan · v{plan.version}</span>
        <span className={`plan-badge ${badge.tone}`}>{badge.label}</span>
      </header>

      {validation && (
        <div className="plan-meta">
          <span>
            Model <code>{validation.modelRoot || "workspace"}</code>
          </span>
          {validation.diffId && (
            <span>
              Diff <code>{validation.diffId}</code>
            </span>
          )}
        </div>
      )}

      {summary && <p className="plan-summary">{summary}</p>}

      {plan.steps.trim() && (
        <div className="plan-steps">
          <MarkdownText text={plan.steps} onOpenFile={onOpenFile} />
        </div>
      )}

      {views.length > 0 && (
        <div className="plan-views">
          <span className="plan-label">Affected views</span>
          <ul>
            {views.map((view) => (
              <li key={view.id}>
                <code>{view.id}</code> {view.title}
                {view.nodes.added + view.nodes.removed + view.nodes.modified > 0 && (
                  <span className="plan-view-count">
                    {view.nodes.added > 0 && ` +${view.nodes.added}`}
                    {view.nodes.removed > 0 && ` -${view.nodes.removed}`}
                    {view.nodes.modified > 0 && ` ~${view.nodes.modified}`}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {notes.length > 0 && (
        <ul className="plan-notes">
          {notes.map((note, index) => (
            <li key={`${note.tone}-${index}`} className={note.tone}>
              {note.text}
            </li>
          ))}
        </ul>
      )}

      {validation?.dsl && (
        <details className="plan-dsl">
          <summary>LikeC4 diff</summary>
          <div className="plan-dsl-body">
            <button type="button" className="plan-copy" onClick={copyDsl}>
              {copied ? "Copied" : "Copy DSL"}
            </button>
            <pre>
              <code>{validation.dsl}</code>
            </pre>
          </div>
        </details>
      )}

      {plan.status === "pending" && (
        <div className="plan-actions">
          <button
            type="button"
            className="primary"
            disabled={!confirmable}
            onClick={() => void confirm()}
          >
            {confirming ? "Starting…" : confirmLabel(plan)}
          </button>
          <button type="button" className="ghost" onClick={onRevise}>
            Continue editing
          </button>
          {plan.validation?.status === "invalid" && (
            <span className="plan-hint">
              Fix the diagnostics above, then validate the draft again.
            </span>
          )}
        </div>
      )}
    </section>
  );
}
