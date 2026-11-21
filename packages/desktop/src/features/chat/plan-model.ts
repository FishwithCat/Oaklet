import type { Item } from "./chat-model.ts";

export type PlanChangeCounts = {
  elements: number;
  relations: number;
};

export type PlanModifiedCounts = {
  elements: number;
};

export type PlanViewCounts = {
  added: number;
  removed: number;
  modified: number;
};

export type PlanDiffView = {
  id: string;
  title: string;
  nodes: PlanViewCounts;
  edges: PlanViewCounts;
};

export type PlanDiffSummary = {
  id: string;
  title: string;
  description?: string | null;
  added: PlanChangeCounts;
  removed: PlanChangeCounts;
  modified: PlanModifiedCounts;
  views: PlanDiffView[];
};

export type PlanDiagnostic = {
  message: string;
  file: string | null;
  line: number | null;
  column: number | null;
};

export type PlanValidation = {
  status: "verified" | "invalid" | "unavailable";
  valid: boolean;
  modelRoot: string;
  includeIgnored: boolean;
  sourceDigest: string;
  diffId: string | null;
  dsl: string;
  diff: PlanDiffSummary | null;
  diagnostics: PlanDiagnostic[];
  warnings: PlanDiagnostic[];
  overlaps: string[];
  error: string | null;
};

export type PlanItem = {
  kind: "plan";
  version: number;
  status: "pending" | "accepted";
  /** Set by the transcript projection for the newest plan version. */
  latest?: boolean;
  steps: string;
  validation?: PlanValidation | null;
  seq?: number;
};

export type PlanBadgeTone = "verified" | "warning" | "error" | "muted" | "accepted";

export type PlanBadge = {
  label: string;
  tone: PlanBadgeTone;
};

export function latestPlanVersion(items: readonly Item[]): number | null {
  let latest: number | null = null;
  for (const item of items) {
    if (item.kind === "plan") {
      latest = latest === null ? item.version : Math.max(latest, item.version);
    }
  }
  return latest;
}

/** The card state that history restoration and the plan event share. */
export function planBadge(
  plan: PlanItem,
  latestVersion: number | null,
): PlanBadge {
  if (plan.status === "accepted") {
    return { label: "Approved", tone: "accepted" };
  }
  if (latestVersion !== null && plan.version < latestVersion) {
    return { label: `Superseded by v${latestVersion}`, tone: "muted" };
  }
  if (plan.validation?.status === "invalid") {
    return { label: "Not verified", tone: "error" };
  }
  if (plan.validation?.status === "unavailable") {
    return { label: "Could not verify", tone: "warning" };
  }
  if (plan.validation?.status === "verified") {
    return { label: "Verified", tone: "verified" };
  }
  return { label: "Text plan", tone: "muted" };
}

/** Only the newest pending version with a valid (or text-only) plan can run. */
export function canConfirmPlan(
  plan: PlanItem,
  latestVersion: number | null,
  running: boolean,
): boolean {
  if (running || plan.status !== "pending") return false;
  if (latestVersion !== null && plan.version !== latestVersion) return false;
  return plan.validation?.status !== "invalid";
}

export function confirmLabel(plan: PlanItem): string {
  return plan.validation?.status === "verified"
    ? "Implement plan"
    : "Implement text plan";
}

function count(value: number, singular: string, plural = `${singular}s`): string {
  return `${value} ${value === 1 ? singular : plural}`;
}

/** "1 element and 1 relation added · 1 removed · 2 modified". */
export function planSummary(validation: PlanValidation): string {
  const diff = validation.diff;
  if (!diff) return "";
  const parts: string[] = [];
  const added = [
    diff.added.elements > 0 ? count(diff.added.elements, "element") : "",
    diff.added.relations > 0 ? count(diff.added.relations, "relation") : "",
  ].filter(Boolean);
  if (added.length > 0) parts.push(`${added.join(" and ")} added`);
  const removed = [
    diff.removed.elements > 0 ? count(diff.removed.elements, "element") : "",
    diff.removed.relations > 0 ? count(diff.removed.relations, "relation") : "",
  ].filter(Boolean);
  if (removed.length > 0) parts.push(`${removed.join(" and ")} removed`);
  if (diff.modified.elements > 0) {
    parts.push(`${count(diff.modified.elements, "element")} modified`);
  }
  return parts.join(" · ");
}

export function planViews(validation: PlanValidation): PlanDiffView[] {
  return validation.diff?.views ?? [];
}

export function planDiagnosticLocation(diagnostic: PlanDiagnostic): string {
  if (!diagnostic.file) return "Model";
  return diagnostic.line === null
    ? diagnostic.file
    : `${diagnostic.file}:${diagnostic.line}${
        diagnostic.column === null ? "" : `:${diagnostic.column}`
      }`;
}

export type PlanNote = { tone: "warning" | "error"; text: string };

/** Diagnostics, overlap warnings, and a failed validation reason, deduplicated. */
export function planNotes(plan: PlanItem): PlanNote[] {
  const validation = plan.validation;
  if (!validation) return [];
  const notes: PlanNote[] = [];
  const seen = new Set<string>();
  const add = (tone: PlanNote["tone"], text: string) => {
    const trimmed = text.trim();
    if (!trimmed || seen.has(trimmed)) return;
    seen.add(trimmed);
    notes.push({ tone, text: trimmed });
  };
  if (validation.error && validation.status !== "verified") {
    add("error", validation.error);
  }
  for (const diagnostic of validation.diagnostics) {
    add("error", `${planDiagnosticLocation(diagnostic)}: ${diagnostic.message}`);
  }
  for (const warning of validation.warnings) {
    add("warning", warning.message);
  }
  if (validation.overlaps.length > 0) {
    add("warning", `Overlaps existing diffs: ${validation.overlaps.join(", ")}`);
  }
  return notes;
}
