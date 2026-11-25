import assert from "node:assert/strict";
import test from "node:test";
import {
  contextSegmentWidths,
  contextTier,
  countUpValue,
  formatCacheHitRate,
  formatContextLabel,
  formatContextPercent,
  formatContextUsage,
  formatModelId,
  formatRequestTitle,
  formatTokens,
  formatUsageTokens,
  formatUsageTitle,
  formatWorkdir,
  isAtBottom,
  isBlankTab,
  mergeDraft,
  shouldShowContextGauge,
  settleStreaming,
  stickAfterScroll,
  toBlocks,
} from "../src/features/chat/chat-model.ts";

const reasoning = (text = "thinking") => ({
  kind: "reasoning",
  runId: "run",
  text,
  streaming: false,
});

const tool = (id, status = "ok") => ({
  kind: "tool",
  runId: "run",
  id,
  name: "read_file",
  status,
});

test("consecutive reasoning and tool items form one trailing process group", () => {
  const items = [reasoning(), tool("1"), reasoning("more")];
  const blocks = toBlocks(items);

  assert.deepEqual(blocks, [{ kind: "process", start: 0, items }]);
});

test("messages separate process groups while preserving original indices", () => {
  const items = [
    { kind: "user", text: "hi" },
    reasoning(),
    tool("1"),
    { kind: "assistant", text: "done", streaming: false },
    reasoning("again"),
    tool("2", "error"),
  ];
  const blocks = toBlocks(items);

  assert.deepEqual(blocks, [
    { kind: "single", index: 0, item: items[0] },
    { kind: "process", start: 1, items: [items[1], items[2]] },
    { kind: "single", index: 3, item: items[3] },
    { kind: "process", start: 4, items: [items[4], items[5]] },
  ]);
});

test("settleStreaming clears only streaming assistant and reasoning items", () => {
  const settledAssistant = { kind: "assistant", text: "old", streaming: false };
  const streamingAssistant = { kind: "assistant", text: "new", streaming: true };
  const settledReasoning = reasoning("settled");
  const streamingReasoning = { ...reasoning("streaming"), streaming: true };
  const user = { kind: "user", text: "hi" };
  const pendingTool = tool("1", "pending");
  const items = [
    settledAssistant,
    streamingAssistant,
    settledReasoning,
    streamingReasoning,
    user,
    pendingTool,
  ];

  const next = settleStreaming(items);

  assert.deepEqual(next, [
    settledAssistant,
    { ...streamingAssistant, streaming: false },
    settledReasoning,
    { ...streamingReasoning, streaming: false },
    user,
    pendingTool,
  ]);
  assert.equal(next[0], settledAssistant);
  assert.equal(next[2], settledReasoning);
  assert.equal(next[4], user);
  assert.equal(next[5], pendingTool);
});

test("settling thinking freezes its duration and tolerates clock corrections", () => {
  const item = { ...reasoning(), streaming: true, startedAt: 1000 };
  const [settled] = settleStreaming([item], 2500);
  assert.equal(settled.durationMs, 1500);
  assert.equal(settled.startedAt, undefined);
  assert.deepEqual(settleStreaming([settled], 9000), [settled]);
  assert.equal(settleStreaming([item], 900)[0].durationMs, 0);
});

test("isBlankTab only accepts an idle, unused tab", () => {
  const blank = { running: false, items: [] };

  assert.equal(isBlankTab(blank), true);
  assert.equal(isBlankTab({ ...blank, running: true }), false);
  assert.equal(
    isBlankTab({ ...blank, items: [{ kind: "notice", tone: "muted", text: "Stopped." }] }),
    false,
  );
});

test("merging a prompt preserves an existing composer draft", () => {
  assert.equal(mergeDraft("", "generated"), "generated");
  assert.equal(mergeDraft("my notes\n\n", "generated"), "my notes\n\ngenerated");
  assert.equal(
    mergeDraft("my notes\n\ngenerated", "generated"),
    "my notes\n\ngenerated",
  );
  assert.equal(mergeDraft("  ", "generated"), "generated");
});

test("context usage reads as a percent with one decimal below 10%", () => {
  assert.equal(formatContextPercent(0, 262144), "0%");
  assert.equal(formatContextPercent(100, 262144), "<0.1%");
  assert.equal(formatContextPercent(1600, 262144), "0.6%");
  assert.equal(formatContextPercent(12400, 262144), "4.7%");
  assert.equal(formatContextPercent(26214, 262144), "10%");
  assert.equal(formatContextPercent(209715, 262144), "80%");
  assert.equal(formatContextPercent(1000, 0), "0%");
});

test("the gauge tooltip pairs the percent with compact token counts", () => {
  assert.equal(formatContextUsage(1600, 262144), "0.6% (1.6k/256k)");
  assert.equal(formatContextUsage(78643, 262144), "30% (77k/256k)");
  assert.equal(formatContextUsage(262144, 262144), "100% (256k/256k)");
  assert.equal(formatTokens(999), "999");
  assert.equal(formatTokens(1024), "1.0k");
  assert.equal(formatTokens(1_572_864), "1.5M");
});

test("the context label reports the context size against the window", () => {
  const usage = {
    promptTokens: 1_400_000,
    completionTokens: 48_000,
    cacheHitTokens: 1_218_000,
    cacheMissTokens: 182_000,
    steps: 60,
  };
  const step = {
    promptTokens: 40_000,
    completionTokens: 900,
    cacheHitTokens: 38_000,
    cacheMissTokens: 2_000,
    steps: 1,
  };
  assert.equal(formatCacheHitRate(usage), "87%");
  assert.equal(formatUsageTokens(1_218_000), "1.2M");
  assert.equal(formatUsageTokens(48_000), "48k");
  assert.equal(formatUsageTokens(999), "999");
  assert.equal(formatContextLabel(41_398, 262_144), "40k/256k");
  assert.equal(formatContextLabel(999, 262_144), "999/256k");
  assert.equal(
    formatRequestTitle(step),
    "last request · cache 95% · 38k/40k prompt · 900 completion",
  );
  assert.equal(
    formatUsageTitle(usage),
    "60 steps · cache 87% · 1.2M/1.4M prompt · 48k completion",
  );
  assert.equal(formatCacheHitRate({ ...usage, cacheHitTokens: 0, cacheMissTokens: 0 }), "0%");
  assert.match(formatUsageTitle({ ...usage, steps: 1 }), /^1 step · /);
});

test("the count-up eases toward the new token total", () => {
  assert.equal(countUpValue(0, 1000, 0), 0);
  assert.equal(countUpValue(0, 1000, 0.5), 875);
  assert.equal(countUpValue(0, 1000, 1), 1000);
  assert.equal(countUpValue(0, 1000, 2), 1000);
  assert.equal(countUpValue(0, 1000, -1), 0);
  assert.equal(countUpValue(5600, 5600, 0.5), 5600);
});

test("the workdir shortens under the home directory", () => {
  assert.equal(
    formatWorkdir("/Users/linear/Workspace/Acorn", "/Users/linear"),
    "~/Workspace/Acorn",
  );
  assert.equal(formatWorkdir("/Users/linear", "/Users/linear"), "~");
  assert.equal(
    formatWorkdir("/Volumes/Work/Acorn", "/Users/linear"),
    "/Volumes/Work/Acorn",
  );
  assert.equal(
    formatWorkdir("/Users/linear2/Acorn", "/Users/linear"),
    "/Users/linear2/Acorn",
  );
  assert.equal(
    formatWorkdir("C:\\Users\\me\\project", "C:\\Users\\me"),
    "~/project",
  );
  assert.equal(
    formatWorkdir("/Users/linear/Workspace/Acorn", ""),
    "/Users/linear/Workspace/Acorn",
  );
});

test("the context plant steps color at 30%, 60%, and 80%", () => {
  assert.equal(contextTier(29, 100), "normal");
  assert.equal(contextTier(30, 100), "elevated");
  assert.equal(contextTier(59, 100), "elevated");
  assert.equal(contextTier(60, 100), "high");
  assert.equal(contextTier(79, 100), "high");
  assert.equal(contextTier(80, 100), "critical");
  assert.equal(contextTier(10, 0), "normal");
});

test("context segments fill their share of the window and stop at the edge", () => {
  const segments = [{ tokens: 3000 }, { tokens: 1000 }];
  assert.deepEqual(
    contextSegmentWidths(segments, 8000).map((width) => Math.round(width)),
    [38, 13],
  );
  const crowded = contextSegmentWidths(
    [{ tokens: 6000 }, { tokens: 4000 }],
    5000,
  );
  assert.equal(
    crowded.reduce((sum, width) => sum + width, 0),
    100,
  );
  assert.deepEqual(contextSegmentWidths([{ tokens: 10 }], 0), [0]);
  assert.deepEqual(contextSegmentWidths([], 1000), []);
});

test("the context plant stays visible for restored sessions without usage", () => {
  assert.equal(shouldShowContextGauge(null, true), true);
  assert.equal(shouldShowContextGauge(null, false), false);
  assert.equal(
    shouldShowContextGauge({ used: 1600, window: 262144 }, false),
    true,
  );
});

test("the composer model label keeps only the last provider segment", () => {
  assert.equal(formatModelId("zai-org/GLM-5.3-Flash"), "GLM-5.3-Flash");
  assert.equal(formatModelId("deepseek-flash"), "deepseek-flash");
  assert.equal(formatModelId("openrouter/anthropic/claude"), "claude");
  assert.equal(formatModelId("provider/"), "provider/");
  assert.equal(formatModelId(""), "");
});

test("the thread only counts as stuck at the true bottom", () => {
  const viewport = (scrollHeight) => ({
    scrollHeight,
    scrollTop: 800,
    clientHeight: 200,
  });
  assert.equal(isAtBottom(viewport(1000)), true);
  assert.equal(isAtBottom(viewport(1002)), true);
  // A few pixels up must detach; an 80px band let the next chunk snap back.
  assert.equal(isAtBottom(viewport(1005)), false);
});

test("a pin that stops at the padding edge reads as detached", () => {
  // .chat keeps 10px of bottom padding and .thread another 8px. scrollIntoView
  // cannot cover them, so the pin used to stop 18px short and detach itself.
  const element = { scrollHeight: 1000, scrollTop: 782, clientHeight: 200 };
  assert.equal(isAtBottom(element), false);
  assert.equal(isAtBottom({ ...element, scrollTop: 800 }), true);
});

test("only an upward move away from the bottom detaches the thread", () => {
  // The pin and streamed growth fire scroll events without moving up.
  assert.equal(stickAfterScroll(true, true, 800, 800), true);
  assert.equal(stickAfterScroll(true, false, 820, 820), true);
  assert.equal(stickAfterScroll(true, false, 760, 800), false);
  // Coming back down re-attaches only once the true bottom is reached.
  assert.equal(stickAfterScroll(false, false, 780, 760), false);
  assert.equal(stickAfterScroll(false, true, 800, 780), true);
  // Content shrinking clamps the offset down but lands on the bottom.
  assert.equal(stickAfterScroll(true, true, 600, 800), true);
});
