import { useMemo, useState } from "react";
import type { FileDiff } from "./process-model";
import { DiffLineView } from "../../shared/DiffLineView";
import { buildDiffRows } from "./tool-diff";

/** Body rows shown before a long diff folds into a "show more" control. */
const MAX_VISIBLE_ROWS = 60;

/** Inline diff card for one file-mutation tool call. */
export function DiffCard({ diffs }: { diffs: readonly FileDiff[] }) {
  const rows = useMemo(() => buildDiffRows(diffs), [diffs]);
  const [expanded, setExpanded] = useState(false);

  if (rows.length === 0) return null;
  const hidden = rows.length - MAX_VISIBLE_ROWS;
  const visible = hidden > 0 && !expanded ? rows.slice(0, MAX_VISIBLE_ROWS) : rows;

  return (
    <div className="tool-diff">
      <div className="tool-diff-scroll">
        {visible.map((row, index) => {
          if (row.kind === "path") {
            return (
              <div className="tool-diff-path" key={index}>
                <span className="tool-diff-path-name">{row.path}</span>
                <span className="diff-stat">
                  <span className="add">+{row.added}</span>{" "}
                  <span className="del">-{row.removed}</span>
                </span>
              </div>
            );
          }
          if (row.kind === "gap") {
            return (
              <div className="tool-diff-gap" key={index} aria-hidden="true">
                ⋯
              </div>
            );
          }
          return <DiffLineView key={index} line={row} wrap />;
        })}
      </div>
      {hidden > 0 && (
        <button
          type="button"
          className="tool-diff-more"
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded ? "Show less" : `Show ${hidden} more lines`}
        </button>
      )}
    </div>
  );
}
