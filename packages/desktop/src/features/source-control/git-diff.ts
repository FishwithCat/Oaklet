import { qualifiedSymbol, supportsOutline, type SourceSymbol } from "../../shared/source-symbol.ts";

export type DiffLine = {
  kind: "add" | "del" | "context" | "meta";
  text: string;
  oldLine: number | null;
  newLine: number | null;
};

type LineRange = { start: number; count: number };

export type DiffHunk = {
  header: string;
  old: LineRange;
  new: LineRange;
  lines: DiffLine[];
};

export type ContextRange = { oldStart: number; newStart: number; count: number };

export type DiffFile = {
  path: string;
  header: string;
  indexLine?: string;
  hasSourceOutline?: boolean;
  added: number;
  removed: number;
  binary: boolean;
  hunks: DiffHunk[];
};

const IMAGE_PATH = /\.(png|jpe?g|gif|webp)$/i;

/** Image formats the core's `read_image` tool can hand to the webview. */
export function isImagePath(path: string): boolean {
  return IMAGE_PATH.test(path);
}

/** A line in the new file that differs from its previous version. */
export type ChangedLine = { line: number; kind: "added" | "modified" };

const HUNK = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

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
        header: rest,
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

    const range = raw.startsWith("@@") ? HUNK.exec(raw) : null;
    if (range) {
      oldLine = Number(range[1]);
      newLine = Number(range[3]);
      hunk = {
        header: raw,
        old: { start: oldLine, count: Number(range[2] ?? 1) },
        new: { start: newLine, count: Number(range[4] ?? 1) },
        lines: [],
      };
      file.hunks.push(hunk);
      continue;
    }

    if (!hunk) {
      if (raw.startsWith("Binary files ")) file.binary = true;
      if (raw.startsWith("index ")) file.indexLine = raw.slice("index ".length);
      if ((raw.startsWith("--- ") || raw.startsWith("+++ ")) &&
          supportsOutline(raw.slice(4).replace(/"?\t?$/, ""))) file.hasSourceOutline = true;
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

export function affectedSymbols(
  hunks: readonly DiffHunk[],
  old: readonly SourceSymbol[],
  next: readonly SourceSymbol[],
): string[][] {
  const findOld = symbolLookup(old);
  const findNew = symbolLookup(next);
  return hunks.map((hunk) => {
    const names = new Set<string>();
    for (const line of hunk.lines) {
      const symbol = line.kind === "add" && line.newLine !== null ? findNew(line.newLine) :
        line.kind === "del" && line.oldLine !== null ? findOld(line.oldLine) : null;
      if (symbol) names.add(qualifiedSymbol(symbol));
    }
    return [...names];
  });
}

function symbolLookup(symbols: readonly SourceSymbol[]) {
  let index = 0;
  let active: SourceSymbol[] = [];
  return (line: number) => {
    active = active.filter((symbol) => symbol.endLine >= line);
    while (index < symbols.length && symbols[index].startLine <= line) {
      const symbol = symbols[index++];
      if (symbol.endLine >= line) active.push(symbol);
    }
    let closest: SourceSymbol | null = null;
    for (const symbol of active) {
      if (!closest || symbol.endLine - symbol.startLine <= closest.endLine - closest.startLine) {
        closest = symbol;
      }
    }
    return closest;
  };
}
export function pageHunks(hunks: DiffHunk[], offset: number, limit: number) {
  const page: (DiffHunk & { index: number; offset: number })[] = [];
  let start = 0;
  for (const [index, hunk] of hunks.entries()) {
    const from = Math.max(0, offset - start);
    const lines = hunk.lines.slice(from, Math.max(0, offset + limit - start));
    if (lines.length) page.push({ ...hunk, lines, index, offset: from });
    start += hunk.lines.length;
    if (start >= offset + limit) break;
  }
  return page;
}

export function hunkOffsets(hunks: readonly DiffHunk[]) {
  let lineCount = 0;
  const offsets = hunks.map(({ lines }) => {
    const offset = lineCount;
    lineCount += lines.length;
    return offset;
  });
  return { offsets, lineCount };
}

export function contextGap(
  before: DiffHunk | undefined,
  after: DiffHunk | undefined,
  oldLength: number,
  newLength: number,
): ContextRange | null {
  const end = ({ start, count }: LineRange) => start + Math.max(1, count);
  const first = ({ start, count }: LineRange) => start + Number(count === 0);
  const oldStart = before ? end(before.old) : 1;
  const newStart = before ? end(before.new) : 1;
  const oldEnd = after ? first(after.old) : oldLength + 1;
  const newEnd = after ? first(after.new) : newLength + 1;
  const count = oldEnd - oldStart;
  return count > 0 && count === newEnd - newStart ? { oldStart, newStart, count } : null;
}

/**
 * New-file lines to mark in an editor gutter. A block that replaces removed
 * lines reads as modified, so a rewritten block is one color like in VS Code.
 */
export function changedLines(files: readonly DiffFile[]): ChangedLine[] {
  const marks: ChangedLine[] = [];
  for (const file of files) {
    for (const hunk of file.hunks) {
      let replacing = false;
      for (const line of hunk.lines) {
        if (line.kind === "context") replacing = false;
        else if (line.kind === "del") replacing = true;
        else if (line.kind === "add" && line.newLine !== null) {
          marks.push({ line: line.newLine, kind: replacing ? "modified" : "added" });
        }
      }
    }
  }
  return marks;
}
