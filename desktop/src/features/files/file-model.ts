export type FileEntry = { name: string; path: string; directory: boolean };

export function fileTreeRows(
  directories: Record<string, FileEntry[]>,
  expanded: ReadonlySet<string>,
) {
  const rows: { entry: FileEntry; depth: number; parent: string; position: number; siblings: number }[] = [];
  function visit(parent: string, depth: number) {
    const entries = directories[parent] ?? [];
    for (const [index, entry] of entries.entries()) {
      rows.push({ entry, depth, parent, position: index + 1, siblings: entries.length });
      if (entry.directory && expanded.has(entry.path)) visit(entry.path, depth + 1);
    }
  }
  if (expanded.has(".")) visit(".", 0);
  return rows;
}

export function lineSeparator(text: string): string {
  return text.includes("\r\n") && !/(?<!\r)\n/.test(text) ? "\r\n" : "\n";
}

export function editorText(text: string): string {
  return lineSeparator(text) === "\r\n" ? text.replace(/\r\n/g, "\n") : text;
}

export function savedText(content: string, original: string): string {
  return lineSeparator(original) === "\r\n" ? content.replace(/\n/g, "\r\n") : content;
}
