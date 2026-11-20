import assert from "node:assert/strict";
import test from "node:test";
import { terminalTheme } from "../src/features/terminal/terminal-theme.ts";

const colors = {
  "--bg": "#ffffff",
  "--text": "#1d3027",
  "--accent": "#1a5319",
  "--terminal-selection": "#d6e8ca",
};

test("the terminal background is the opaque app background", () => {
  const theme = terminalTheme((property) => colors[property] ?? "");
  assert.equal(theme.background, "#ffffff");
  assert.equal(theme.cursorAccent, "#ffffff");
});

test("colors are trimmed and the selection stays visible", () => {
  const theme = terminalTheme((property) => ` ${colors[property] ?? ""}\n`);
  assert.equal(theme.foreground, "#1d3027");
  assert.equal(theme.selectionBackground, "#d6e8ca");
});
