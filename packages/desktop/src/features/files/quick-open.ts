/** Split a workspace-relative path into the filename and its folder label. */
export function fileParts(path: string): { name: string; dir: string } {
  const parts = path.split("/").filter(Boolean);
  const name = parts.pop() ?? path;
  return { name, dir: parts.join("/") };
}

/** Gaps between matched characters; null when the query's characters are missing. */
function fuzzy(text: string, query: string): number | null {
  let at = 0;
  let previous = -1;
  let gaps = 0;
  for (const char of query) {
    const found = text.indexOf(char, at);
    if (found === -1) return null;
    if (previous >= 0) gaps += found - previous - 1;
    previous = found;
    at = found + 1;
  }
  return gaps;
}

/**
 * Rank workspace files for a quick-open query. Filename matches beat path
 * matches, and contiguous or earlier matches beat scattered ones.
 */
export function matchFiles(
  files: readonly string[],
  query: string,
  limit = 100,
): string[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return files.slice(0, limit);

  const scored: { path: string; score: number }[] = [];
  for (const path of files) {
    const name = fileParts(path).name.toLowerCase();
    const lower = path.toLowerCase();
    const nameAt = name.indexOf(needle);
    let score: number | null = null;
    if (nameAt === 0) score = 0;
    else if (nameAt > 0) score = 10 + nameAt;
    else {
      const nameFuzzy = fuzzy(name, needle);
      if (nameFuzzy !== null) score = 100 + nameFuzzy;
      else {
        const pathAt = lower.indexOf(needle);
        if (pathAt >= 0) score = 200 + pathAt;
        else {
          const pathFuzzy = fuzzy(lower, needle);
          if (pathFuzzy !== null) score = 400 + pathFuzzy;
        }
      }
    }
    if (score !== null) scored.push({ path, score });
  }
  scored.sort((left, right) => left.score - right.score || left.path.localeCompare(right.path));
  return scored.slice(0, limit).map(({ path }) => path);
}
