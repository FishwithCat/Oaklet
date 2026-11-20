import { RangeSet, StateEffect, StateField } from "@codemirror/state";
import { GutterMarker, gutterLineClass } from "@uiw/react-codemirror";
import type { ChangedLine } from "../source-control/git-diff";

const addedMarker = new (class extends GutterMarker { elementClass = "cm-line-added"; })();
const modifiedMarker = new (class extends GutterMarker { elementClass = "cm-line-modified"; })();

export const setChangedLines = StateEffect.define<readonly ChangedLine[]>();

/**
 * Uncommitted lines, drawn in the line-number gutter like VS Code. Positions
 * map through edits, so marks stay on their lines while typing; the next
 * fetched diff replaces them.
 */
export const changedLinesField = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update(marks, tr) {
    for (const effect of tr.effects) {
      if (!effect.is(setChangedLines)) continue;
      const doc = tr.state.doc;
      const ranges = effect.value
        .filter(({ line }) => line >= 1 && line <= doc.lines)
        .map(({ line, kind }) => (kind === "added" ? addedMarker : modifiedMarker).range(doc.line(line).from));
      return RangeSet.of(ranges, true);
    }
    if (!tr.docChanged) return marks;
    const doc = tr.state.doc;
    const snapped: ReturnType<GutterMarker["range"]>[] = [];
    for (const iter = marks.map(tr.changes).iter(0); iter.value; iter.next()) {
      snapped.push(iter.value.range(doc.lineAt(iter.from).from));
    }
    return RangeSet.of(snapped, true);
  },
  provide: (field) => gutterLineClass.from(field),
});
