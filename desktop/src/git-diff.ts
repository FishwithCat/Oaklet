export type DiffLine = {
  kind: "add" | "del" | "context" | "meta";
  text: string;
  oldLine: number | null;
  newLine: number | null;
};

export type DiffHunk = { header: string; lines: DiffLine[] };

export type DiffFile = {
  path: string;
  added: number;
  removed: number;
  binary: boolean;
  hunks: DiffHunk[];
};

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

function unquotePath(path: string): string {
  if (!path.startsWith('"') || !path.endsWith('"')) return path;
  try {
    return JSON.parse(path) as string;
  } catch {
    return path.slice(1, -1);
  }
}

/** Split `git show --patch` output into files, hunks, and line numbers. */
export function parseDiff(text: string): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | null = null;
  let hunk: DiffHunk | null = null;
  let oldLine = 0;
  let newLine = 0;

  for (const raw of text.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const rest = raw.slice("diff --git ".length);
      const split = rest.lastIndexOf(" b/");
      file = {
        path: unquotePath(split >= 0 ? rest.slice(split + 3) : rest),
        added: 0,
        removed: 0,
        binary: false,
        hunks: [],
      };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) continue;

    if (raw.startsWith("@@")) {
      const match = HUNK.exec(raw);
      oldLine = match ? Number(match[1]) : 0;
      newLine = match ? Number(match[2]) : 0;
      hunk = { header: raw, lines: [] };
      file.hunks.push(hunk);
      continue;
    }

    if (!hunk) {
      if (raw.startsWith("Binary files ")) file.binary = true;
      continue;
    }

    if (raw.startsWith("\\ No newline")) {
      hunk.lines.push({ kind: "meta", text: raw, oldLine: null, newLine: null });
    } else if (raw.startsWith("+")) {
      hunk.lines.push({ kind: "add", text: raw.slice(1), oldLine: null, newLine });
      newLine += 1;
      file.added += 1;
    } else if (raw.startsWith("-")) {
      hunk.lines.push({ kind: "del", text: raw.slice(1), oldLine, newLine: null });
      oldLine += 1;
      file.removed += 1;
    } else if (raw.startsWith(" ")) {
      hunk.lines.push({ kind: "context", text: raw.slice(1), oldLine, newLine });
      oldLine += 1;
      newLine += 1;
    }
  }

  return files;
}
