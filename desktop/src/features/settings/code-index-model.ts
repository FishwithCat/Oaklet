import type { CodeIndexConfig } from "./config";

export type IndexLanguage = "rust" | "typescript" | "python" | "go" | "java";
export type IndexProject = {
  root: string;
  language: IndexLanguage;
  state: string | { Failed: string };
  version: string | null;
  capabilities: Record<string, boolean> | null;
  progress: string[];
  generation: number;
};
export type IndexStatus = {
  workspace: string;
  revision: number;
  stale: boolean;
  discovering: boolean;
  incomplete: boolean;
  warnings: string[];
  projects: IndexProject[];
  cache: { entries: number; bytes: number; error: string | null; semantic_cache_enabled: boolean };
  persistence: null | {
    verifying: boolean;
    extracting: boolean;
    restored_projects: number;
    total_files: number;
    files: number;
    extracted_files: number;
    reused_files: number;
    skipped_files: number;
    error: string | null;
    projects: { root: string; language: IndexLanguage; verified: boolean }[];
  };
};

export function stateText(state: IndexProject["state"]): string {
  if (typeof state !== "string") return `Failed: ${state.Failed}`;
  switch (state) {
    case "Ready": return "Protocol ready · semantic completeness unknown";
    case "Queued": return "Queued · waiting for server resources";
    case "Starting": return "Starting language server…";
    case "Idle": return "Idle · starts again on query";
    default: return state;
  }
}

export function parseIndexArgs(text: string): string[] {
  const args: unknown = JSON.parse(text);
  if (!Array.isArray(args) || !args.every((arg) => typeof arg === "string" && !arg.includes("\0"))) {
    throw new Error("Arguments must be a JSON array of strings without NUL characters.");
  }
  return args;
}

export function updateIndexServer(config: CodeIndexConfig, language: IndexLanguage, patch: NonNullable<CodeIndexConfig["servers"][IndexLanguage]>): CodeIndexConfig {
  return { servers: { ...config.servers, [language]: { ...config.servers[language], ...patch } } };
}

/** Subscribe before reading; one revision order covers events and the initial snapshot. */
export function subscribeIndex(
  listen: (receive: (status: IndexStatus) => void) => Promise<() => void>,
  snapshot: () => Promise<IndexStatus>,
  receive: (status: IndexStatus) => void,
  fail: (error: unknown) => void,
): () => void {
  let disposed = false;
  let revision = -1;
  let release: (() => void) | undefined;
  const accept = (next: IndexStatus) => {
    if (disposed || next.revision <= revision) return;
    revision = next.revision;
    receive(next);
  };
  void (async () => {
    try {
      const unlisten = await listen(accept);
      if (disposed) { unlisten(); return; }
      release = unlisten;
      accept(await snapshot());
    } catch (error) {
      if (!disposed) fail(error);
    }
  })();
  return () => { disposed = true; release?.(); };
}
