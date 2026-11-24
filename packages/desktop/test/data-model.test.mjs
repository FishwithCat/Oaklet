import assert from "node:assert/strict";
import test from "node:test";
import {
  formatBytes,
  formatCount,
  sessionsLabel,
  totalBytes,
} from "../src/features/settings/data-model.ts";

test("formats byte sizes with one unit", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1024), "1.0 KB");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(10 * 1024), "10 KB");
  assert.equal(formatBytes(1024 * 1024), "1.0 MB");
  assert.equal(formatBytes(2.5 * 1024 * 1024), "2.5 MB");
  assert.equal(formatBytes(1024 ** 3), "1.0 GB");
  assert.equal(formatBytes(2 * 1024 ** 4), "2.0 TB");
});

test("clamps invalid and negative sizes", () => {
  assert.equal(formatBytes(-1), "0 B");
  assert.equal(formatBytes(Number.NaN), "0 B");
  assert.equal(formatCount(Number.NaN), "0");
});

test("adds the database and spill sizes", () => {
  assert.equal(totalBytes({ databaseBytes: 1024, spillBytes: 512 }), 1536);
  assert.equal(totalBytes({ databaseBytes: 0, spillBytes: 0 }), 0);
});

test("groups counts and labels sessions", () => {
  assert.equal(formatCount(1234567), "1,234,567");
  assert.equal(sessionsLabel(1), "1 session");
  assert.equal(sessionsLabel(0), "0 sessions");
  assert.equal(sessionsLabel(2048), "2,048 sessions");
});
