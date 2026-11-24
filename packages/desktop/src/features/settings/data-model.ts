export type WorkspaceUsage = {
  workspace: string;
  sessions: number;
  messages: number;
  bytes: number;
  updatedAt: number;
};

export type StorageUsage = {
  databaseBytes: number;
  spillBytes: number;
  spillFiles: number;
  sessions: number;
  messages: number;
  workspaces: WorkspaceUsage[];
};

export type PurgeResult = {
  deleted: number;
  usage: StorageUsage;
};

const UNITS = ["KB", "MB", "GB", "TB"];

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return "0 B";
  const value = Math.max(0, Math.round(bytes));
  if (value < 1024) return `${value} B`;
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size < 10 ? size.toFixed(1) : Math.round(size)} ${UNITS[unit]}`;
}

export function formatCount(value: number): string {
  if (!Number.isFinite(value)) return "0";
  return Math.max(0, Math.round(value)).toLocaleString("en-US");
}

export function totalBytes(usage: Pick<StorageUsage, "databaseBytes" | "spillBytes">): number {
  return Math.max(0, usage.databaseBytes) + Math.max(0, usage.spillBytes);
}

export function sessionsLabel(count: number): string {
  return `${formatCount(count)} ${count === 1 ? "session" : "sessions"}`;
}
