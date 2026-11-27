import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { FlowerpotRegular } from "@mingcute/react/core-regular";
import {
  contextTier,
  countUpValue,
  formatContextLabel,
  formatContextUsage,
  formatRequestTitle,
  formatUsageTitle,
} from "./chat-model";
import { ContextInspector } from "./ContextInspector";
import type { ContextUsage, RunUsage, StepUsage } from "./session-model";

function UsageLabel({ used, limit }: { used: number; limit: number }) {
  const [shown, setShown] = useState(used);
  const shownRef = useRef(used);

  useEffect(() => {
    const from = shownRef.current;
    if (from === used) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      shownRef.current = used;
      setShown(used);
      return;
    }
    const started = performance.now();
    let frame = 0;
    const tick = (now: number) => {
      const value = countUpValue(from, used, (now - started) / 450);
      shownRef.current = value;
      setShown(value);
      if (value !== used) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [used]);

  return <>{formatContextLabel(shown, limit)}</>;
}

function ContextGauge({
  usage,
  runUsage,
  running,
  compacting,
  onCompact,
}: {
  usage: ContextUsage | null;
  runUsage: RunUsage | null;
  running: boolean;
  compacting: boolean;
  onCompact: () => void;
}) {
  const label = usage ? formatContextUsage(usage.used, usage.window) : null;
  const tokens =
    runUsage && runUsage.promptTokens > 0
      ? formatUsageTitle(runUsage)
      : null;
  const tier = usage ? contextTier(usage.used, usage.window) : "normal";
  const hint = running
    ? "Stop the run to compact context"
    : compacting
      ? "Click to stop compacting"
      : usage
        ? "Click to compact context"
        : "Context usage is unavailable; click to compact context";
  return (
    <button
      type="button"
      className={`context-meter${tier === "normal" ? "" : ` ${tier}`}${
        compacting ? " compacting" : ""
      }`}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onCompact}
      disabled={running}
      aria-label={
        label
          ? `Context used: ${label}.${tokens ? ` ${tokens}.` : ""} ${hint}`
          : `Context usage unavailable.${tokens ? ` ${tokens}.` : ""} ${hint}`
      }
      title={
        label || tokens
          ? `${[label, tokens].filter(Boolean).join(" · ")} — ${hint}`
          : `Context usage unavailable — ${hint}`
      }
    >
      <FlowerpotRegular size={18} aria-hidden="true" />
    </button>
  );
}

export function ComposerContext({
  activeTabId,
  context,
  usage,
  stepUsage,
  running,
  compacting,
  showContextGauge,
  onCompact,
}: {
  activeTabId: string;
  context: ContextUsage | null;
  usage: RunUsage | null;
  stepUsage: StepUsage | null;
  running: boolean;
  compacting: boolean;
  showContextGauge: boolean;
  onCompact: () => void;
}) {
  const [usageOpen, setUsageOpen] = useState(false);
  const usageRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!usageOpen) return;
    function onMouseDown(event: MouseEvent) {
      if (!usageRef.current?.contains(event.target as Node)) {
        setUsageOpen(false);
      }
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setUsageOpen(false);
    }
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [usageOpen]);

  useLayoutEffect(() => {
    setUsageOpen(false);
  }, [activeTabId]);

  const gauge = showContextGauge ? (
    <ContextGauge
      usage={context}
      runUsage={usage}
      running={running}
      compacting={compacting}
      onCompact={onCompact}
    />
  ) : null;
  if (!gauge && !context) return null;

  return (
    <span className="composer-context" ref={usageRef}>
      {gauge}
      {context && (
        <button
          type="button"
          className="composer-usage"
          title={stepUsage ? formatRequestTitle(stepUsage) : "Current context"}
          aria-haspopup="dialog"
          aria-expanded={usageOpen}
          aria-controls={usageOpen ? "context-inspector" : undefined}
          onClick={() => setUsageOpen((open) => !open)}
        >
          <UsageLabel
            key={activeTabId}
            used={context.used}
            limit={context.window}
          />
        </button>
      )}
      {usageOpen && context && <ContextInspector usage={context} />}
    </span>
  );
}
