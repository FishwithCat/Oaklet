import type { FileDiff } from "./process-model";

/** One line the diff card can draw. */
export type DiffRow =
  | { kind: "path"; path: string; added: number; removed: number }
  | { kind: "gap" }
  | { kind: "add"; text: string }
  | { kind: "del"; text: string }
  | { kind: "context"; text: string };

export type DiffTotals = { added: number; removed: number };

/** Guard the quadratic line diff so a large rewrite cannot freeze the UI. */
const MAX_LCS_CELLS = 500_000;

/**
 * Split a side's text into content lines. One trailing newline terminates the
 * last line; an interior blank line survives. CRLF is normalized away so a
 * line-ending change alone does not redraw the whole fragment.
 */
function contentLines(text: string): string[] {
  if (text === "") return [];
  const body = text.endsWith("\n") ? text.slice(0, -1) : text;
  return body.split("\n").map((line) => line.replace(/\r$/, ""));
}

/** Line-level edits from an LCS walk, or a whole-fragment replacement. */
function lineDiff(
  oldLines: readonly string[],
  newLines: readonly string[],
): DiffRow[] {
  if (oldLines.length === 0) {
    return newLines.map((text) => ({ kind: "add" as const, text }));
  }
  if (newLines.length === 0) {
    return oldLines.map((text) => ({ kind: "del" as const, text }));
  }

  if (oldLines.length * newLines.length > MAX_LCS_CELLS) {
    return [
      ...oldLines.map((text): DiffRow => ({ kind: "del", text })),
      ...newLines.map((text): DiffRow => ({ kind: "add", text })),
    ];
  }

  const width = newLines.length + 1;
  const table = new Uint32Array((oldLines.length + 1) * width);
  for (let i = oldLines.length - 1; i >= 0; i -= 1) {
    for (let j = newLines.length - 1; j >= 0; j -= 1) {
      table[i * width + j] =
        oldLines[i] === newLines[j]
          ? table[(i + 1) * width + j + 1]! + 1
          : Math.max(table[(i + 1) * width + j]!, table[i * width + j + 1]!);
    }
  }

  const rows: DiffRow[] = [];
  let i = 0;
  let j = 0;
  while (i < oldLines.length && j < newLines.length) {
    if (oldLines[i] === newLines[j]) {
      rows.push({ kind: "context", text: oldLines[i]! });
      i += 1;
      j += 1;
    } else if (
      table[(i + 1) * width + j]! >= table[i * width + j + 1]!
    ) {
      rows.push({ kind: "del", text: oldLines[i]! });
      i += 1;
    } else {
      rows.push({ kind: "add", text: newLines[j]! });
      j += 1;
    }
  }
  while (i < oldLines.length) {
    rows.push({ kind: "del", text: oldLines[i]! });
    i += 1;
  }
  while (j < newLines.length) {
    rows.push({ kind: "add", text: newLines[j]! });
    j += 1;
  }
  return rows;
}

/**
 * Flatten backend fragments into card rows. A path header opens each file; a
 * gap separates consecutive hunks from the same file.
 */
export function buildDiffRows(diffs: readonly FileDiff[]): DiffRow[] {
  const rows: DiffRow[] = [];
  let lastPath: string | undefined;
  let lastPathRow: Extract<DiffRow, { kind: "path" }> | undefined;
  for (const diff of diffs) {
    const lines = lineDiff(
      contentLines(diff.oldText ?? ""),
      contentLines(diff.newText),
    );
    let added = 0;
    let removed = 0;
    for (const line of lines) {
      if (line.kind === "add") added += 1;
      if (line.kind === "del") removed += 1;
    }

    if (lastPath === diff.path && lastPathRow !== undefined) {
      lastPathRow.added += added;
      lastPathRow.removed += removed;
      if (lines.length > 0) rows.push({ kind: "gap" });
    } else {
      const row: Extract<DiffRow, { kind: "path" }> = {
        kind: "path",
        path: diff.path,
        added,
        removed,
      };
      rows.push(row);
      lastPath = diff.path;
      lastPathRow = row;
    }
    rows.push(...lines);
  }
  return rows;
}

export function diffTotals(diffs: readonly FileDiff[]): DiffTotals {
  let added = 0;
  let removed = 0;
  for (const row of buildDiffRows(diffs)) {
    if (row.kind === "path") {
      added += row.added;
      removed += row.removed;
    }
  }
  return { added, removed };
}
