import {
  contextSegmentWidths,
  formatContextLabel,
  formatContextPercent,
  formatTokens,
} from "./chat-model";
import type {
  ContextSegment,
  ContextSegmentKind,
  ContextUsage,
} from "./session-model";

const SEGMENT_LABELS: Record<ContextSegmentKind, string> = {
  system: "System prompt",
  instructions: "Project instructions",
  skills: "Skills",
  tools: "Tool schemas",
  conversation: "Conversation",
  tool_results: "Tool results",
};

function SegmentRow({ segment }: { segment: ContextSegment }) {
  return (
    <div className="context-inspector-row">
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
    </div>
  );
}

export function ContextInspector({ usage }: { usage: ContextUsage }) {
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
                <SegmentRow segment={segment} />
              </li>
            ))}
          </ul>
        </>
      ) : (
        <p className="context-inspector-note">
          No breakdown for this context yet.
        </p>
      )}
    </div>
  );
}
