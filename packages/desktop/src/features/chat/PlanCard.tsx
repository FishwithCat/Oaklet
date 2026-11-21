import { lazy, Suspense, useEffect, useState } from "react";
import { DownSmallRegular } from "@mingcute/react/core-regular";
import { MarkdownText } from "../../shared/Markdown";
import { errorText } from "../../shared/error-text";
import {
  canConfirmPlan,
  confirmLabel,
  planBadge,
  planHasReviewDetails,
  planImpactGroups,
  planNotes,
  planSummary,
  planViews,
  type PlanBaseline,
  type PlanCompareData,
  type PlanImpactItem,
  type PlanItem,
  type PlanStep,
} from "./plan-model";

const PlanCompare = lazy(() =>
  import("./PlanCompare").then((module) => ({ default: module.PlanCompare })),
);

function stepId(version: number, number: number): string {
  return `plan-v${version}-step-${number}`;
}

function StepDetails({
  step,
  number,
  version,
  open,
  onToggle,
}: {
  step: PlanStep;
  number: number;
  version: number;
  open: boolean;
  onToggle: () => void;
}) {
  const dependencies = step.dependsOn ?? [];
  return (
    <li className={`plan-step${open ? " open" : ""}`} id={stepId(version, number)}>
      <button
        type="button"
        className="plan-step-toggle"
        aria-expanded={open}
        onClick={onToggle}
      >
        <span className="plan-step-number">
          {String(number).padStart(2, "0")}
        </span>
        <span className="plan-step-title">{step.title}</span>
        <DownSmallRegular size={12} className="plan-step-chevron" />
      </button>
      {open && (
        <div className="plan-step-body">
          <div>
            <span className="plan-step-label">Expected result</span>
            <p>{step.outcome}</p>
          </div>
          {(step.files?.length ?? 0) > 0 && (
            <div>
              <span className="plan-step-label">Files</span>
              <ul className="plan-step-chips">
                {step.files?.map((file) => (
                  <li key={file}>
                    <code>{file}</code>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {(step.components?.length ?? 0) > 0 && (
            <div>
              <span className="plan-step-label">Components</span>
              <ul className="plan-step-chips">
                {step.components?.map((component) => (
                  <li key={component}>
                    <code>{component}</code>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {dependencies.length > 0 && (
            <div>
              <span className="plan-step-label">Depends on</span>
              <p>
                {dependencies.map((dependency) => `Step ${dependency}`).join(", ")}
              </p>
            </div>
          )}
          <div>
            <span className="plan-step-label">Verification</span>
            <p>{step.verification}</p>
          </div>
        </div>
      )}
    </li>
  );
}

function ImpactItem({
  item,
  onJump,
}: {
  item: PlanImpactItem;
  onJump: (step: number) => void;
}) {
  const content = (
    <>
      <span className="plan-impact-name">{item.label}</span>
      <span className="plan-impact-detail">{item.detail}</span>
      {item.step !== null && (
        <span className="plan-impact-step">Step {item.step}</span>
      )}
    </>
  );
  return (
    <li className={item.kind}>
      {item.step === null ? (
        <div className="plan-impact-row">{content}</div>
      ) : (
        <button
          type="button"
          className="plan-impact-row"
          onClick={() => onJump(item.step!)}
          title={`Go to step ${item.step}`}
        >
          {content}
        </button>
      )}
    </li>
  );
}

export function PlanCard({
  plan,
  latestVersion,
  running,
  onConfirm,
  onRevise,
  onOpenFile,
  onCheckBaseline,
  onLoadCompare,
}: {
  plan: PlanItem;
  latestVersion: number | null;
  running: boolean;
  onConfirm: (version: number) => Promise<boolean>;
  onRevise: () => void;
  onOpenFile: (href: string) => void;
  onCheckBaseline: (version: number) => Promise<PlanBaseline>;
  onLoadCompare: (version: number, viewId: string) => Promise<PlanCompareData>;
}) {
  const [confirming, setConfirming] = useState(false);
  const [copied, setCopied] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [openSteps, setOpenSteps] = useState<ReadonlySet<number>>(
    () => new Set(),
  );
  const [baseline, setBaseline] = useState({
    checked: false,
    stale: false,
    error: "",
  });
  const validation = plan.validation ?? null;
  const details = plan.details ?? null;
  const badge = planBadge(plan, latestVersion);
  const notes = planNotes(plan);
  const views = validation ? planViews(validation) : [];
  const summary = validation ? planSummary(validation) : "";
  const impacts = planImpactGroups(plan);
  const hasImpactList = Boolean(validation?.diff?.changes);
  const steps = details?.steps ?? [];
  const risks = details?.risks ?? [];
  const questions = details?.questions ?? [];
  const meta = [
    summary,
    views.length > 0
      ? `${views.length} ${views.length === 1 ? "view" : "views"}`
      : "",
  ].filter(Boolean);
  const canShowDetails = planHasReviewDetails(plan);
  const currentPending =
    plan.status === "pending" &&
    plan.version === latestVersion &&
    validation?.status === "verified";
  const confirmable =
    canConfirmPlan(plan, latestVersion, running, baseline.checked && !baseline.stale && !baseline.error) &&
    !confirming;

  useEffect(() => {
    if (!currentPending) return;
    let cancelled = false;
    let checking = false;
    let stale = false;
    let timer: number | undefined;
    async function check() {
      if (cancelled || checking || stale || document.hidden) return;
      window.clearTimeout(timer);
      checking = true;
      try {
        const result = await onCheckBaseline(plan.version);
        if (result.version !== plan.version || !result.checked) {
          throw new Error("Could not verify this plan's architecture baseline.");
        }
        stale = result.baselineChanged;
        if (!cancelled) setBaseline({ checked: true, stale, error: "" });
      } catch (error) {
        if (!cancelled) setBaseline({ checked: true, stale: false, error: errorText(error) });
      } finally {
        checking = false;
        if (!cancelled && !stale) timer = window.setTimeout(check, 3000);
      }
    }
    void check();
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
    };
  }, [
    currentPending,
    latestVersion,
    plan.status,
    plan.version,
    validation?.sourceDigest,
  ]);

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

  function toggleStep(number: number) {
    setOpenSteps((current) => {
      const next = new Set(current);
      if (next.has(number)) next.delete(number);
      else next.add(number);
      return next;
    });
  }

  function jumpToStep(number: number) {
    setOpenSteps((current) => new Set(current).add(number));
    window.requestAnimationFrame(() => {
      document
        .getElementById(stepId(plan.version, number))
        ?.scrollIntoView({ block: "center", behavior: "smooth" });
    });
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

      {meta.length > 0 && (
        <div className="plan-meta">{meta.join(" · ")}</div>
      )}

      {details?.objective && (
        <div className="plan-objective">
          <MarkdownText text={details.objective} onOpenFile={onOpenFile} />
        </div>
      )}

      {steps.length > 0 ? (
        <ol className="plan-step-list">
          {steps.map((step, index) => (
            <StepDetails
              key={`${index}-${step.title}`}
              step={step}
              number={index + 1}
              version={plan.version}
              open={openSteps.has(index + 1)}
              onToggle={() => toggleStep(index + 1)}
            />
          ))}
        </ol>
      ) : (
        plan.steps.trim() && (
          <div className="plan-steps">
            <MarkdownText text={plan.steps} onOpenFile={onOpenFile} />
          </div>
        )
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

      {(risks.length > 0 || questions.length > 0) && (
        <div className="plan-decisions">
          <span className="plan-label">Before implementing</span>
          {risks.length > 0 && (
            <ul className="plan-decision-list risks" aria-label="Risks">
              {risks.map((risk) => (
                <li key={risk}>
                  <span className="plan-decision-mark" aria-hidden="true">
                    !
                  </span>
                  {risk}
                </li>
              ))}
            </ul>
          )}
          {questions.length > 0 && (
            <ul
              className="plan-decision-list questions"
              aria-label="Open questions"
            >
              {questions.map((question) => (
                <li key={question}>
                  <span className="plan-decision-mark" aria-hidden="true">
                    ?
                  </span>
                  {question}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {baseline.stale && (
        <p className="plan-baseline stale" role="alert">
          The architecture model changed after this plan was validated. Ask for
          an updated plan before implementing it.
        </p>
      )}
      {baseline.error && (
        <p className="plan-baseline error" role="status">
          {baseline.error}
        </p>
      )}

      <div className="plan-footer">
        {plan.status === "pending" && (
          <div className="plan-actions">
            <button
              type="button"
              className="primary"
              disabled={!confirmable}
              onClick={() => void confirm()}
            >
              {confirming
                ? "Starting…"
                : currentPending && !baseline.checked
                  ? "Checking model…"
                  : confirmLabel(plan)}
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
        {canShowDetails && (
          <button
            type="button"
            className="plan-details-toggle"
            aria-expanded={detailsOpen}
            onClick={() => setDetailsOpen((value) => !value)}
          >
            {detailsOpen ? "Hide details" : "Review details"}
            <DownSmallRegular size={12} aria-hidden="true" />
          </button>
        )}
      </div>

      {detailsOpen && (
        <div className="plan-details">
          {details?.changes && (
            <div className="plan-overview">
              <span className="plan-label">Changes</span>
              <MarkdownText text={details.changes} onOpenFile={onOpenFile} />
            </div>
          )}

          {validation && (hasImpactList || validation.status === "verified") && (
            <div className="plan-impact">
              <span className="plan-label">Architecture impact</span>
              {impacts.length === 0 ? (
                <p className="plan-impact-empty">
                  {hasImpactList
                    ? "The validated diff reports no element or relation changes."
                    : "This saved plan has no impact list. Use the affected views and DSL below."}
                </p>
              ) : (
                impacts.map((group) => (
                  <div key={group.kind} className={`plan-impact-group ${group.kind}`}>
                    <div className="plan-impact-head">
                      <span className="plan-impact-symbol">{group.symbol}</span>
                      <span>{group.label}</span>
                      <span className="plan-impact-count">{group.items.length}</span>
                    </div>
                    <ul>
                      {group.items.map((item) => (
                        <ImpactItem key={item.key} item={item} onJump={jumpToStep} />
                      ))}
                    </ul>
                    <details className="plan-impact-raw">
                      <summary>IDs</summary>
                      <ul>
                        {group.items.map((item) => (
                          <li key={item.key}>
                            <code>{item.id}</code>
                            <span>{item.label}</span>
                          </li>
                        ))}
                      </ul>
                    </details>
                  </div>
                ))
              )}
            </div>
          )}

          {views.length > 0 && validation?.status === "verified" && (
            <Suspense
              fallback={
                <p className="plan-compare-status" role="status">
                  Loading comparison…
                </p>
              }
            >
              <PlanCompare plan={plan} onLoad={onLoadCompare} />
            </Suspense>
          )}
          {views.length > 0 && validation?.status !== "verified" && (
            <div className="plan-views">
              <span className="plan-label">Affected views</span>
              <ul>
                {views.map((view) => (
                  <li key={view.id}>
                    <code>{view.id}</code> {view.title}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {validation?.dsl && (
            <details className="plan-dsl">
              <summary>Raw LikeC4 diff</summary>
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

          {validation && (validation.modelRoot || validation.diffId) && (
            <div className="plan-trace">
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
        </div>
      )}
    </section>
  );
}
