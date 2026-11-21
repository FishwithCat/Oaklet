import assert from "node:assert/strict";
import test from "node:test";
import {
  canConfirmPlan,
  confirmLabel,
  latestPlanVersion,
  planBadge,
  planNotes,
  planSummary,
  planViews,
} from "../src/features/chat/plan-model.ts";

const validation = (overrides = {}) => ({
  status: "verified",
  valid: true,
  modelRoot: "architecture",
  includeIgnored: false,
  sourceDigest: "abc123",
  diffId: "worker",
  dsl: "diff worker 'Add worker' {}",
  diff: {
    id: "worker",
    title: "Add worker",
    added: { elements: 1, relations: 1 },
    removed: { elements: 0, relations: 0 },
    modified: { elements: 0 },
    views: [
      {
        id: "containers",
        title: "Containers",
        nodes: { added: 1, removed: 0, modified: 0 },
        edges: { added: 1, removed: 0, modified: 0 },
      },
    ],
  },
  diagnostics: [],
  warnings: [],
  overlaps: [],
  error: null,
  ...overrides,
});

const plan = (overrides = {}) => ({
  kind: "plan",
  version: 1,
  status: "pending",
  steps: "1. Add the worker.",
  validation: validation(),
  ...overrides,
});

test("the newest plan version drives which card is actionable", () => {
  const first = plan({ seq: 1 });
  const second = plan({ version: 2, seq: 2 });
  assert.equal(latestPlanVersion([first, second]), 2);
  assert.equal(latestPlanVersion([{ kind: "assistant", text: "hi", streaming: false }]), null);

  assert.equal(canConfirmPlan(first, 2, false), false);
  assert.equal(canConfirmPlan(second, 2, false), true);
  assert.equal(canConfirmPlan(second, 2, true), false);
  assert.deepEqual(planBadge(first, 2), {
    label: "Superseded by v2",
    tone: "muted",
  });
});

test("validation state changes the badge and confirmation label", () => {
  assert.deepEqual(planBadge(plan(), 1), { label: "Verified", tone: "verified" });
  assert.equal(confirmLabel(plan()), "Implement plan");

  const invalid = plan({ validation: validation({ status: "invalid", valid: false }) });
  assert.deepEqual(planBadge(invalid, 1), { label: "Not verified", tone: "error" });
  assert.equal(canConfirmPlan(invalid, 1, false), false);
  assert.equal(confirmLabel(invalid), "Implement text plan");

  const unavailable = plan({
    validation: validation({ status: "unavailable", valid: false }),
  });
  assert.deepEqual(planBadge(unavailable, 1), {
    label: "Could not verify",
    tone: "warning",
  });
  assert.equal(canConfirmPlan(unavailable, 1, false), true);
  assert.equal(confirmLabel(unavailable), "Implement text plan");

  const textOnly = plan({ validation: null });
  assert.deepEqual(planBadge(textOnly, 1), { label: "Text plan", tone: "muted" });
  assert.equal(canConfirmPlan(textOnly, 1, false), true);
  assert.equal(confirmLabel(textOnly), "Implement text plan");
});

test("accepted cards stay visible and cannot start another run", () => {
  const accepted = plan({ status: "accepted" });
  assert.deepEqual(planBadge(accepted, 1), {
    label: "Approved",
    tone: "accepted",
  });
  assert.equal(canConfirmPlan(accepted, 1, false), false);
});

test("summaries and affected views come from the upstream diff", () => {
  const summary = planSummary(validation());
  assert.equal(summary, "1 element and 1 relation added");
  assert.equal(planViews(validation())[0].id, "containers");
  assert.equal(planSummary(validation({ diff: null })), "");
});

test("notes combine diagnostics, warnings, and overlaps without duplicates", () => {
  const notes = planNotes(
    plan({
      validation: validation({
        diagnostics: [
          { message: "unknown element", file: "architecture/model.c4", line: 4, column: 2 },
        ],
        warnings: [{ message: "shared warning", file: null, line: null, column: null }],
        overlaps: ["worker"],
      }),
    }),
  );
  assert.deepEqual(notes, [
    { tone: "error", text: "architecture/model.c4:4:2: unknown element" },
    { tone: "warning", text: "shared warning" },
    { tone: "warning", text: "Overlaps existing diffs: worker" },
  ]);
});
