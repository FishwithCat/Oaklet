import { formatLineSpan, quoteExcerpt } from "../../shared/chat-quote.ts";

type SelectedRow = { oldLine: number; newLine: number; text: string };

export function readDiffSelection(container: HTMLElement | null): SelectedRow[] {
  const selection = container?.ownerDocument.getSelection();
  if (!container || !selection?.rangeCount || selection.isCollapsed) return [];
  const range = selection.getRangeAt(0);
  if (!container.contains(range.commonAncestorContainer)) return [];

  const rows: SelectedRow[] = [];
  for (const cell of container.querySelectorAll<HTMLElement>("[data-old-line], [data-new-line]")) {
    if (!range.intersectsNode(cell)) continue;
    const clipped = range.cloneRange();
    if (!cell.contains(range.startContainer)) clipped.setStart(cell, 0);
    if (!cell.contains(range.endContainer)) clipped.setEnd(cell, cell.childNodes.length);
    const text = clipped.toString();
    // The range can touch the start of the next line without selecting text.
    if (!text && cell.textContent) continue;
    rows.push({
      oldLine: Number(cell.dataset.oldLine ?? 0),
      newLine: Number(cell.dataset.newLine ?? 0),
      text,
    });
  }
  return rows;
}

function spans(rows: readonly SelectedRow[], side: "oldLine" | "newLine"): string {
  const ranges: [number, number][] = [];
  for (const row of rows) {
    const line = row[side];
    if (!line) continue;
    const last = ranges[ranges.length - 1];
    if (last && line === last[1] + 1) last[1] = line;
    else ranges.push([line, line]);
  }
  return ranges.map(([first, last]) => formatLineSpan(first, last)).join(", ");
}

export function quoteDiffSelection(path: string, oldPath: string | null, rows: readonly SelectedRow[]): string {
  const oldLines = spans(rows, "oldLine");
  const newLines = spans(rows, "newLine");
  const source = [
    oldLines && `Old ${oldPath ?? path}:${oldLines}`,
    newLines && `New ${path}:${newLines}`,
  ].filter(Boolean).join("\n");
  const text = rows.map(({ oldLine, newLine, text }) => {
    const sign = oldLine === 0 ? "+" : newLine === 0 ? "-" : " ";
    return `${sign}${text}`;
  }).join("\n");
  return quoteExcerpt(source, text);
}
