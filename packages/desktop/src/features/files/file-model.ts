export type FileEntry = { name: string; path: string; directory: boolean };
export type FileChange = { path: string; status: string };

const WINDOWS_ABSOLUTE = /^(?:[A-Za-z]:[\\/]|\\\\)/;

export function isWindowsPath(path: string): boolean {
  return WINDOWS_ABSOLUTE.test(path);
}

export function workspaceName(workspace: string): string {
  const parts = workspace
    .split(isWindowsPath(workspace) ? /[\\/]/ : "/")
    .filter(Boolean);
  return parts.pop() || workspace;
}

/** Path segments shown in the editor breadcrumbs, starting at the workspace. */
export function breadcrumbParts(workspace: string, path: string): string[] {
  return [workspaceName(workspace), ...path.split("/").filter(Boolean)];
}

export function absolutePath(workspace: string, path: string): string {
  const windows = isWindowsPath(workspace);
  const root = workspace.replace(windows ? /[\\/]+$/ : /\/+$/, "");
  const relative = windows ? path.replace(/\//g, "\\") : path;
  return `${root}${windows ? "\\" : "/"}${relative}`;
}

export function treeKey(path: string): string {
  return path.split("/").filter(Boolean).join("/") || ".";
}

/**
 * Index changes by file path and collect every ancestor folder, so collapsed
 * folders can still show that something below them changed.
 */
export function changeMarkers(changes: readonly FileChange[]) {
  const files = new Map<string, FileChange>();
  const folders = new Set<string>();
  for (const change of changes) {
    const path = treeKey(change.path);
    if (path === ".") continue;
    files.set(path, change);
    const parts = path.split("/");
    for (let index = 1; index < parts.length; index++) {
      folders.add(parts.slice(0, index).join("/"));
    }
  }
  return { files, folders };
}

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

export function isMarkdownPath(path: string): boolean {
  return /\.(md|mdx)$/i.test(path);
}
