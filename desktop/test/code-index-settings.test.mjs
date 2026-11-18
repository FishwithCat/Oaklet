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
const { CodeIndexSettings, ServerFields, WorkspaceIndexStatus } = await vite.ssrLoadModule("/src/features/settings/CodeIndexSettings.tsx");
after(() => vite.close());

test("index settings render labelled per-language fields and installation requirements", () => {
  const html = renderToStaticMarkup(React.createElement(CodeIndexSettings, {
    config: { servers: { rust: { enabled: false, command: "/tools/rust analyzer", args: ["--literal"] } } },
    disabled: false,
    onChange() {},
    onValidity() {},
  }));
  assert.match(html, /Save applies server settings to every open workspace/);
  for (const language of ["rust", "typescript", "python", "go", "java"]) {
    assert.match(html, new RegExp(`for="index-command-${language}"`));
    assert.match(html, new RegExp(`for="index-args-${language}"`));
  }
  assert.match(html, /\/tools\/rust analyzer/);
  assert.match(html, /--literal/);
  assert.match(html, /role="status"/);
  assert.match(html, /<fieldset style="min-width:0;margin:0;padding:12px;border:1px solid var\(--border\);border-radius:8px">/);
  assert.match(html, /<details open=""><summary style="[^"]+">Server configuration<\/summary>/);
  assert.match(html, /<div style="display:grid;grid-template-columns:minmax\(96px, 140px\) minmax\(0, 1fr\);align-items:center;column-gap:12px;row-gap:4px/);
  assert.doesNotMatch(html, /class="index-server"/);
  assert.ok(html.indexOf("Current workspace") < html.indexOf("Language servers"));
  assert.doesNotMatch(html, />Install<|type="submit"/);
});


test("workspace status shows the workspace path, disk counts and rebuild action", () => {
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
    disabled: false, busy: false, onRebuild() {},
  }));
  assert.match(html, /class="index-metrics"/);
  assert.match(html, /<dt>Files checked<\/dt><dd>225 \/ 225<\/dd>/);
  assert.equal((html.match(/class="index-path"/g) ?? []).length, 1);
  assert.match(html, />Clear and rebuild index<\/button>/);
  assert.doesNotMatch(html, /<article|rust-analyzer|>Retry<\/button>/);
});

test("language server settings show one set of actions for multiple projects", () => {
  const projects = ["/workspace", "/workspace/python"].map((root) => ({
    root, language: "python", state: { Failed: "Code index executable not found: pyright-langserver" },
  }));
  const renderServer = (installation, servers = {}, currentProjects = projects) => renderToStaticMarkup(React.createElement(ServerFields, {
    language: { id: "python", name: "Python", command: "pyright-langserver" }, projects: currentProjects, installation,
    config: { servers }, onChange() {}, onValidity() {}, onRetry() {}, onInstall() {}, onCancelInstall() {},
  }));

  const html = renderServer();
  assert.equal((html.match(/>Install server<\/button>/g) ?? []).length, 1);
  assert.equal((html.match(/>Retry<\/button>/g) ?? []).length, 1);
  assert.equal((html.match(/Code index executable not found/g) ?? []).length, 1);
  assert.doesNotMatch(html, /\/workspace/);
  assert.match(html, /<input type="checkbox" disabled="" style="[^"]*"\/>/);
  assert.match(renderServer(undefined, {}, [projects[0], { ...projects[1], state: "Ready" }]),
    /<input type="checkbox" style="[^"]*" checked=""\/>/);

  const verifying = renderServer("Verifying");
  assert.match(verifying, /Checking server version and initialization/);
  assert.match(verifying, />Cancel installation<\/button>/);
  assert.doesNotMatch(verifying, />Install server<\/button>/);

  const failed = renderServer({ Failed: "Network unavailable" });
  assert.match(failed, /Network unavailable/);
  assert.match(failed, />Retry installation<\/button>/);
  assert.doesNotMatch(renderServer(undefined, { python: { command: "/custom/pyright" } }), />Install server<\/button>/);
});
