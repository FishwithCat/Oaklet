export type CommitInfo = {
  hash: string;
  shortHash: string;
  author: string;
  email: string;
  date: string;
  subject: string;
  refs: string[];
  body: string;
};

export type GitLog = {
  branch: string;
  branches: string[];
  commits: CommitInfo[];
  hasTrackedChanges: boolean;
};

export type ReviewFile = {
  path: string;
  status: string;
  oldPath?: string | null;
  added?: number | null;
  removed?: number | null;
};

export type ChangeInfo = ReviewFile & { unstaged: boolean };

export function filterCommits(commits: readonly CommitInfo[], query: string): readonly CommitInfo[] {
  const term = query.trim().toLowerCase();
  if (!term) return commits;
  const words = term.split(/\s+/);
  return commits.filter((commit) => {
    const text = [commit.hash, commit.shortHash, commit.subject, commit.body, commit.author, commit.email, ...commit.refs]
      .join("\n").toLowerCase();
    return words.every((word) => text.includes(word));
  });
}

export function sortReviewFiles(files: readonly ReviewFile[]): ReviewFile[] {
  const byPath = new Map<string, ReviewFile>();
  for (const file of files) {
    if (!byPath.has(file.path)) byPath.set(file.path, file);
  }
  return [...byPath.values()].sort((a, b) => a.path < b.path ? -1 : Number(a.path > b.path));
}

export function filterReviewFiles(files: readonly ReviewFile[], query: string): ReviewFile[] {
  const term = query.trim().toLowerCase();
  return files.filter(({ path, oldPath }) =>
    path.toLowerCase().includes(term) || oldPath?.toLowerCase().includes(term),
  );
}

export function chooseReviewPath(files: readonly ReviewFile[], selected: string | null): string | null {
  if (!selected) return files[0]?.path ?? null;
  return files.find((file) => file.path >= selected)?.path ?? files[files.length - 1]?.path ?? null;
}

export function filterBranches(
  branches: readonly string[],
  query: string,
): string[] {
  const term = query.trim().toLowerCase();
  if (!term) return [...branches];
  return branches.filter((branch) => branch.toLowerCase().includes(term));
}

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
