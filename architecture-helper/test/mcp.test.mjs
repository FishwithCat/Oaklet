import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const helper = fileURLToPath(new URL("../src/mcp.mjs", import.meta.url));
const fixture = fileURLToPath(new URL("fixtures/valid", import.meta.url));

function query(request, workspace = fixture) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [helper, workspace], { timeout: 15_000 });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) return reject(new Error(stderr || `Exit ${code}`));
      try { resolve(JSON.parse(stdout)); } catch (error) { reject(error); }
    });
    child.stdin.end(JSON.stringify(request));
  });
}

test("discovers official tool schemas within the agent's output limit", async () => {
  const result = await query({});
  assert.ok(JSON.stringify(result).length < 30_000);
  const read = result.tools.find((tool) => tool.name === "read-element");
  assert.ok(read.description);
  assert.ok(read.inputSchema.properties.id);
});

test("queries a real project and its elements over MCP", async () => {
  const listed = await query({ tool: "list-projects" });
  assert.equal(listed.structuredContent.projects[0].id, "oaklet-fixture");
  const summary = await query({ tool: "read-project-summary", arguments: { project: "oaklet-fixture" } });
  assert.equal(summary.isError, undefined);
  assert.ok(summary.structuredContent.elements.length > 0);
});

test("preserves MCP tool errors for unknown tools and invalid arguments", async () => {
  assert.equal((await query({ tool: "nonexistent" })).isError, true);
  assert.equal((await query({ tool: "read-element", arguments: {} })).isError, true);
});

test("an empty workspace remains queryable", async () => {
  const workspace = fileURLToPath(new URL("fixtures/no-model", import.meta.url));
  const result = await query({ tool: "list-projects" }, workspace);
  assert.notEqual(result.isError, true);
});
