import assert from "node:assert/strict";
import test from "node:test";
import { resolveTheme } from "../src/features/settings/theme.ts";

test("auto follows the system while explicit themes win", () => {
  assert.equal(resolveTheme("auto", true), "dark");
  assert.equal(resolveTheme("auto", false), "light");
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("dark", false), "dark");
});
