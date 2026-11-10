export type CommitInfo = {
  hash: string;
  shortHash: string;
  author: string;
  email: string;
  date: string;
  subject: string;
  body: string;
};

export type GitLog = {
  branch: string;
  branches: string[];
  commits: CommitInfo[];
};

export type ChangeInfo = { path: string; status: string; unstaged: boolean };

const CHANGE_STATUS: Record<string, { label: string; title: string }> = {
  untracked: { label: "U", title: "Untracked" },
  added: { label: "A", title: "Added" },
  modified: { label: "M", title: "Modified" },
  deleted: { label: "D", title: "Deleted" },
  renamed: { label: "R", title: "Renamed" },
  copied: { label: "C", title: "Copied" },
  typechange: { label: "T", title: "Type changed" },
  conflicted: { label: "!", title: "Conflicted" },
};

export function statusInfo(status: string) {
  return CHANGE_STATUS[status] ?? { label: "M", title: "Modified" };
}

export function relativeDate(iso: string): string {
  const time = new Date(iso).getTime();
  if (Number.isNaN(time)) return iso;

  const elapsed = (Date.now() - time) / 1000;
  const units = [
    ["year", 31536000],
    ["month", 2592000],
    ["week", 604800],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ] as const;
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

  for (const [unit, seconds] of units) {
    if (Math.abs(elapsed) >= seconds) {
      return format.format(-Math.round(elapsed / seconds), unit);
    }
  }
  return "just now";
}

export function fullDate(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString();
}
