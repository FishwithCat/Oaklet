export type FindMatch = { start: number; end: number };

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Case-insensitive, non-overlapping matches, as offsets usable for DOM Ranges. */
export function findMatches(text: string, query: string): FindMatch[] {
  if (!query) return [];
  const pattern = new RegExp(escapeRegExp(query), "gi");
  return Array.from(text.matchAll(pattern), (match) => ({
    start: match.index ?? 0,
    end: (match.index ?? 0) + match[0].length,
  }));
}

/** Advance through matches, wrapping at both ends. */
export function stepMatch(index: number, count: number, delta: number): number {
  return count === 0 ? 0 : (index + delta + count) % count;
}

export type TextRuns = { starts: number[]; lengths: number[] };

/** Map a concatenated-text offset back to the run that contains it. */
export function locateOffset(
  runs: TextRuns,
  offset: number,
): { index: number; offset: number } | null {
  let low = 0;
  let high = runs.starts.length - 1;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (runs.starts[middle] <= offset) low = middle;
    else high = middle - 1;
  }
  const start = runs.starts[low];
  const length = runs.lengths[low];
  if (start === undefined || length === undefined) return null;
  return { index: low, offset: Math.min(offset - start, length) };
}
