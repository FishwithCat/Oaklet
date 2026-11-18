import assert from "node:assert/strict";
import test, { after } from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const vite = await createServer({
  configFile: false,
  server: { middlewareMode: true },
  appType: "custom",
  logLevel: "silent",
});
const { CodeIndexSettings, WorkspaceIndexStatus } = await vite.ssrLoadModule("/src/features/settings/CodeIndexSettings.tsx");
after(() => vite.close());

test("index settings render labelled per-language fields and installation requirements", () => {
  const html = renderToStaticMarkup(React.createElement(CodeIndexSettings, {
    config: { servers: { rust: { enabled: false, command: "/tools/rust analyzer", args: ["--literal"] } } },
    disabled: false,
    onChange() {},
    onValidity() {},
  }));
  assert.match(html, /Servers must be installed separately/);
  assert.match(html, /Save applies server settings to every open workspace/);
  for (const language of ["rust", "typescript"]) {
    assert.match(html, new RegExp(`for="index-command-${language}"`));
    assert.match(html, new RegExp(`for="index-args-${language}"`));
  }
  assert.match(html, /\/tools\/rust analyzer/);
  assert.match(html, /--literal/);
  assert.match(html, /role="status"/);
  assert.match(html, /<details class="index-server-options" open="">/);
  assert.match(html, /<details class="index-server-options"><summary>Server configuration<\/summary>/);
  assert.ok(html.indexOf("Current workspace") < html.indexOf("Language servers"));
  assert.doesNotMatch(html, />Install<|type="submit"/);
});


test("workspace status groups counts, project metadata and actions without repeating paths in headings", () => {
  const root = "/Users/linear/Workspace/Acorn";
  const html = renderToStaticMarkup(React.createElement(WorkspaceIndexStatus, {
    status: {
      workspace: root, revision: 1, stale: false, discovering: false, incomplete: false, warnings: [],
      cache: { entries: 0, bytes: 0, error: null, semantic_cache_enabled: false },
      persistence: {
        verifying: false, extracting: false, restored_projects: 0, total_files: 225, files: 225,
        extracted_files: 59, reused_files: 0, skipped_files: 166, error: null,
        projects: [{ root, language: "rust", verified: true }],
      },
      projects: [{
        root, language: "rust", state: "Ready", version: "rust-analyzer 1.99.1", progress: ["request (lib)"],
        capabilities: { symbols: true, workspace_symbols: true, definition: true, references: true, calls: true }, generation: 1,
      }],
    },
    disabled: false, busy: false, onRetry() {}, onRebuild() {},
  }));
  assert.match(html, /class="index-metrics"/);
  assert.match(html, /<dt>Files checked<\/dt><dd>225 \/ 225<\/dd>/);
  assert.match(html, /<h4>Rust<\/h4>/);
  assert.match(html, /class="index-path"/);
  assert.match(html, /File bytes verified/);
  assert.match(html, /rust-analyzer 1.99.1/);
  assert.match(html, /Semantic completeness remains unknown/);
  assert.match(html, />Retry<\/button>/);
  assert.match(html, />Clear and rebuild index<\/button>/);
  assert.doesNotMatch(html, /<h4>rust ·/);
});
