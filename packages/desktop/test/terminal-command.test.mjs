import assert from "node:assert/strict";
import test from "node:test";
import { commandForTerminal } from "../src/features/terminal/terminal-command.ts";

test("a selected sentence becomes a shell command followed by Enter", () => {
  assert.equal(commandForTerminal("  ls -la  "), "ls -la\r");
});

test("trailing whitespace and newlines collapse into one Enter", () => {
  assert.equal(commandForTerminal("ls -la\n"), "ls -la\r");
  assert.equal(commandForTerminal("ls -la\r\n"), "ls -la\r");
});

test("blank selections are ignored", () => {
  assert.equal(commandForTerminal("  \n "), null);
});
