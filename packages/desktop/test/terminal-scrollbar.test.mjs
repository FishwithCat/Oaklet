import assert from "node:assert/strict";
import test from "node:test";
import { scrollbarGeometry } from "../src/features/terminal/terminal-scrollbar.ts";

test("no scrollback hides the scrollbar", () => {
  assert.deepEqual(scrollbarGeometry(0, 24, 24, 400), {
    hidden: true,
    top: 0,
    height: 0,
  });
});

test("the thumb reaches the bottom edge at the end of the buffer", () => {
  const track = 400;
  const atTop = scrollbarGeometry(0, 20, 120, track);
  const atBottom = scrollbarGeometry(100, 20, 120, track);

  assert.equal(atTop.top, 0);
  assert.equal(atTop.height, Math.round((20 / 120) * track));
  assert.equal(atBottom.top + atBottom.height, track);
});

test("the thumb stays inside the track and above the minimum size", () => {
  const huge = scrollbarGeometry(0, 1, 50_000, 400);
  assert.equal(huge.height, 24);

  const past = scrollbarGeometry(999, 20, 120, 400);
  assert.equal(past.top + past.height, 400);
});
