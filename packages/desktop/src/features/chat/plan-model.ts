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
  changes?: PlanDiffChanges | null;
};

export type PlanElementChange = {
  id: string;
  title: string;
  kind?: string | null;
};

export type PlanRelationChange = {
  id: string;
  source: string;
  sourceTitle: string;
  target: string;
  targetTitle: string;
  label: string;
};

export type PlanModifiedElementChange = PlanElementChange & {
  fields: string[];
};

export type PlanDiffChanges = {
  added: {
    elements: PlanElementChange[];
    relations: PlanRelationChange[];
  };
  removed: {
    elements: PlanElementChange[];
    relations: PlanRelationChange[];
  };
  modified: {
    elements: PlanModifiedElementChange[];
  };
};

export type PlanDiagnostic = {
  message: string;
  file: string | null;
  line: number | null;
  column: number | null;
};

export type PlanValidation = {
  status: "required" | "verified" | "invalid" | "unavailable";
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
  details?: PlanDetails | null;
  validation?: PlanValidation | null;
  seq?: number;
};

export type PlanStep = {
  title: string;
  outcome: string;
  files?: string[];
  components?: string[];
  dependsOn?: number[];
  verification: string;
};

export type PlanDetails = {
  objective: string;
  changes: string;
  steps: PlanStep[];
  risks?: string[];
  questions?: string[];
};

export type PlanBaseline = {
  version: number;
  checked: boolean;
  baselineChanged: boolean;
};

export type PlanCompareData = {
  version: number;
  modelRoot: string;
  sourceDigest: string;
  viewId: string;
  model: unknown;
  view: unknown;
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
  if (plan.validation?.status === "required") {
    return { label: "Validation required", tone: "warning" };
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
  baselineCurrent: boolean,
): boolean {
  if (running || plan.status !== "pending") return false;
  if (latestVersion !== null && plan.version !== latestVersion) return false;
  if (plan.validation?.status === "verified") {
    return plan.validation.valid && baselineCurrent;
  }
  return !plan.validation || plan.validation.status === "unavailable";
}

export function confirmLabel(plan: PlanItem): string {
  if (plan.validation?.status === "required") return "Validate before implementing";
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

export type PlanImpactKind = "added" | "modified" | "removed";

export type PlanImpactItem = {
  key: string;
  /** Model element or relation ID, shown in the expandable raw details. */
  id: string;
  kind: PlanImpactKind;
  label: string;
  detail: string;
  refs: string[];
  /** One-based step number when a step names this element or relation. */
  step: number | null;
};

export type PlanImpactGroup = {
  kind: PlanImpactKind;
  label: string;
  symbol: string;
  items: PlanImpactItem[];
};

const IMPACT_GROUPS: Omit<PlanImpactGroup, "items">[] = [
  { kind: "added", label: "Added", symbol: "+" },
  { kind: "modified", label: "Modified", symbol: "~" },
  { kind: "removed", label: "Removed", symbol: "−" },
];

function stepNumber(
  refs: readonly string[],
  steps: readonly PlanStep[],
): number | null {
  const wanted = new Set(refs.filter(Boolean));
  const index = steps.findIndex((step) =>
    (step.components ?? []).some((component) => wanted.has(component)),
  );
  return index === -1 ? null : index + 1;
}

function relationRefs(change: PlanRelationChange): string[] {
  return [
    change.id,
    change.label,
    `${change.source} -> ${change.target}`,
    `${change.sourceTitle} -> ${change.targetTitle}`,
  ].filter(Boolean);
}

function relationDetail(change: PlanRelationChange): string {
  const route = `${change.sourceTitle || change.source} → ${
    change.targetTitle || change.target
  }`;
  return change.label ? `${route} · ${change.label}` : route;
}

/** Architecture changes grouped the way the review checklist renders them. */
export function planImpactGroups(plan: PlanItem): PlanImpactGroup[] {
  const changes = plan.validation?.diff?.changes;
  if (!changes) return [];
  const steps = plan.details?.steps ?? [];
  const groups: PlanImpactGroup[] = [
    {
      ...IMPACT_GROUPS[0]!,
      items: changes.added.elements.map((element) => {
        const refs = [element.id, element.title];
        return {
          key: `added-element-${element.id}`,
          id: element.id,
          kind: "added" as const,
          label: element.title || element.id,
          detail: element.kind ?? "Element",
          refs,
          step: stepNumber(refs, steps),
        };
      }),
    },
    {
      ...IMPACT_GROUPS[1]!,
      items: changes.modified.elements.map((element) => {
        const refs = [element.id, element.title];
        return {
          key: `modified-element-${element.id}`,
          id: element.id,
          kind: "modified" as const,
          label: element.title || element.id,
          detail:
            element.fields.length > 0
              ? element.fields.join(", ")
              : "Element updated",
          refs,
          step: stepNumber(refs, steps),
        };
      }),
    },
    {
      ...IMPACT_GROUPS[2]!,
      items: changes.removed.elements.map((element) => {
        const refs = [element.id, element.title];
        return {
          key: `removed-element-${element.id}`,
          id: element.id,
          kind: "removed" as const,
          label: element.title || element.id,
          detail: element.kind ?? "Element",
          refs,
          step: stepNumber(refs, steps),
        };
      }),
    },
  ];
  // Relations sit beside the elements they connect in the same status group.
  groups[0]!.items.push(
    ...changes.added.relations.map((relation) => {
      const refs = relationRefs(relation);
      return {
        key: `added-relation-${relation.id}`,
        id: relation.id,
        kind: "added" as const,
        label: relation.label || relationDetail(relation),
        detail: relationDetail(relation),
        refs,
        step: stepNumber(refs, steps),
      };
    }),
  );
  groups[2]!.items.push(
    ...changes.removed.relations.map((relation) => {
      const refs = relationRefs(relation);
      return {
        key: `removed-relation-${relation.id}`,
        id: relation.id,
        kind: "removed" as const,
        label: relation.label || relationDetail(relation),
        detail: relationDetail(relation),
        refs,
        step: stepNumber(refs, steps),
      };
    }),
  );
  return groups.filter((group) => group.items.length > 0);
}

export function planHasReviewDetails(plan: PlanItem): boolean {
  return Boolean(
    plan.details?.changes?.trim() ||
      plan.validation?.dsl ||
      plan.validation?.diff,
  );
}

export type CompareMode = "changes" | "as-is" | "to-be";

export const COMPARE_MODES: { id: CompareMode; label: string }[] = [
  { id: "changes", label: "Changes" },
  { id: "as-is", label: "As-is" },
  { id: "to-be", label: "To-be" },
];
