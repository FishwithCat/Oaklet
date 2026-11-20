export type ArchitectureRuntime = {
  ready: boolean;
  node: string | null;
  nodeVersion: string | null;
  helper: string | null;
  message: string;
};

export type ArchitectureModelRoot = {
  root: string;
  sources: string[];
  config: string | null;
  unsupportedConfig: string | null;
};

export type ArchitectureDiscovery = {
  workspace: string;
  models: ArchitectureModelRoot[];
  defaultPath: string;
  runtime: ArchitectureRuntime;
  warnings: string[];
};

export type ArchitectureSource = {
  path: string;
  bytes: number;
};

export type ArchitectureView = {
  id: string;
  title: string;
  kind: string;
};

export type ArchitectureDiagnostic = {
  message: string;
  file: string | null;
  line: number | null;
  column: number | null;
};

export type ArchitectureLoad = {
  workspace: string;
  root: string;
  sources: ArchitectureSource[];
  views: ArchitectureView[];
  model: unknown | null;
  diagnostics: ArchitectureDiagnostic[];
  stale: boolean;
  cached: boolean;
  sourceDigest: string;
  error: string | null;
  warnings: string[];
};

/** Keep the current root when it still exists; otherwise select the first. */
export function pickModelRoot(
  models: ArchitectureModelRoot[],
  current: string | null,
): string | null {
  if (current && models.some((model) => model.root === current)) return current;
  return models[0]?.root ?? null;
}

/** Keep the current view when it still exists; otherwise select the first. */
export function pickViewId(
  views: ArchitectureView[],
  current: string | null,
): string | null {
  if (current && views.some((view) => view.id === current)) return current;
  return views[0]?.id ?? null;
}

export function diagnosticLocation(diagnostic: ArchitectureDiagnostic): string {
  if (!diagnostic.file) return "Model";
  const position =
    diagnostic.line === null
      ? diagnostic.file
      : `${diagnostic.file}:${diagnostic.line}${
          diagnostic.column === null ? "" : `:${diagnostic.column}`
        }`;
  return position;
}

export function rootLabel(root: string): string {
  return root === "." ? "Workspace root" : root;
}

/**
 * A load response is usable only if it belongs to the newest request and to the
 * workspace that is still selected.
 */
export function isCurrentLoad(
  requestSequence: number,
  latestSequence: number,
  responseWorkspace: string,
  workspace: string,
): boolean {
  return requestSequence === latestSequence && responseWorkspace === workspace;
}
