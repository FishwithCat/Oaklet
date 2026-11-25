import { useState } from "react";
import { DownSmallRegular } from "@mingcute/react/core-regular";
import {
  contextSegmentWidths,
  formatContextLabel,
  formatContextPercent,
  formatRequestTitle,
  formatTokens,
} from "./chat-model";
import type {
  ContextSegment,
  ContextSegmentKind,
  ContextUsage,
  StepUsage,
} from "./session-model";

const SEGMENT_LABELS: Record<ContextSegmentKind, string> = {
  system: "System prompt",
  instructions: "Project instructions",
  skills: "Skills",
  tools: "Tool schemas",
  conversation: "Conversation",
  tool_results: "Tool results",
};

function SegmentRow({
  segment,
  open,
  onToggle,
}: {
  segment: ContextSegment;
  open: boolean;
  onToggle: () => void;
}) {
  const meta = (
    <>
      <span
        className="context-inspector-dot"
        data-kind={segment.kind}
        aria-hidden="true"
      />
      <span className="context-inspector-label">
        {SEGMENT_LABELS[segment.kind]}
      </span>
      <span className="context-inspector-tokens">
        {formatTokens(segment.tokens)}
      </span>
    </>
  );
  if (segment.items.length === 0) {
    return <div className="context-inspector-row">{meta}</div>;
  }
  const listId = `context-items-${segment.kind}`;
  return (
    <>
      <button
        type="button"
        className="context-inspector-row"
        aria-expanded={open}
        aria-controls={listId}
        onClick={onToggle}
      >
        {meta}
        <DownSmallRegular size={10} className="context-inspector-chevron" />
      </button>
      {open && (
        <ul className="context-inspector-items" id={listId}>
          {segment.items.map((item, index) => (
            <li key={`${item.label}-${index}`}>
              <span className="context-inspector-item-label" title={item.label}>
                {item.label}
              </span>
              <span className="context-inspector-tokens">
                {formatTokens(item.tokens)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

export function ContextInspector({
  usage,
  stepUsage,
}: {
  usage: ContextUsage;
  stepUsage: StepUsage | null;
}) {
  const [expanded, setExpanded] = useState<ContextSegmentKind | null>(null);
  const widths = contextSegmentWidths(usage.segments, usage.window);
  return (
    <div
      className="context-inspector"
      id="context-inspector"
      role="dialog"
      aria-label="Context usage"
    >
      <h3>Context Inspector</h3>
      <div className="context-inspector-summary">
        <strong>{formatContextPercent(usage.used, usage.window)}</strong>
        <span>{formatContextLabel(usage.used, usage.window)}</span>
      </div>
      {usage.segments.length > 0 ? (
        <>
          <div className="context-inspector-bar" aria-hidden="true">
            {usage.segments.map((segment, index) => (
              <span
                key={segment.kind}
                data-kind={segment.kind}
                style={{ width: `${widths[index]}%` }}
              />
            ))}
          </div>
          <ul className="context-inspector-rows">
            {usage.segments.map((segment) => (
              <li key={segment.kind}>
                <SegmentRow
                  segment={segment}
                  open={expanded === segment.kind}
                  onToggle={() =>
                    setExpanded((current) =>
                      current === segment.kind ? null : segment.kind,
                    )
                  }
                />
              </li>
            ))}
          </ul>
          <p className="context-inspector-note">
            Estimated split, scaled to the request total.
          </p>
        </>
      ) : (
        <p className="context-inspector-note">
          No breakdown for this context yet.
        </p>
      )}
      {stepUsage && (
        <p className="context-inspector-note">
          {formatRequestTitle(stepUsage)}
        </p>
      )}
    </div>
  );
}
