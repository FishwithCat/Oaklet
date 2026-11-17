import assert from "node:assert/strict";
import test from "node:test";
import { inlineCodeFile, workspaceFileLink } from "../src/shared/file-link.ts";

test("workspace file links keep relative paths and normalize them", () => {
  assert.equal(workspaceFileLink("docs/design.md"), "docs/design.md");
  assert.equal(workspaceFileLink("./docs/design.md"), "docs/design.md");
  assert.equal(workspaceFileLink("docs/./nested/design.md"), "docs/nested/design.md");
  assert.equal(workspaceFileLink("docs\\nested\\design.md"), "docs/nested/design.md");
  assert.equal(workspaceFileLink("docs/My%20Design.md"), "docs/My Design.md");
  assert.equal(workspaceFileLink("docs/design.md#install"), "docs/design.md");
  assert.equal(workspaceFileLink("docs/design.md?plain=1"), "docs/design.md");
  assert.equal(workspaceFileLink("  docs/design.md  "), "docs/design.md");
});

test("non-file and workspace-escaping links are not opened", () => {
  for (const href of [
    "",
    "   ",
    "#install",
    "/etc/passwd",
    "//example.com/design.md",
    "\\\\server\\share\\design.md",
    "../outside.md",
    "docs/../outside.md",
    "docs/",
    "https://example.com/design.md",
    "http://example.com/design.md",
    "mailto:me@example.com",
    "file:///Users/me/design.md",
    "C:\\work\\design.md",
    "docs/%E0%A4%A.md",
  ]) {
    assert.equal(workspaceFileLink(href), null, href);
  }
});

test("inline code names a file when it is a path with a known extension", () => {
  assert.equal(inlineCodeFile("TECH_STACK.md"), "TECH_STACK.md");
  assert.equal(inlineCodeFile("README.md"), "README.md");
  assert.equal(inlineCodeFile("package.json"), "package.json");
  assert.equal(inlineCodeFile("Cargo.toml"), "Cargo.toml");
  assert.equal(inlineCodeFile("src/main.ts"), "src/main.ts");
  assert.equal(inlineCodeFile("index.html"), "index.html");
  assert.equal(inlineCodeFile("./docs/My%20Design.md"), "docs/My Design.md");
});

test("inline code that is not a file path stays plain", () => {
  for (const text of [
    "npm run build",
    "React.Component",
    "v1.2.3",
    "e.g.",
    "process.env.NODE_ENV",
    "foo.bar",
    "Node.js",
    "App.tsx",
    "TECH_STACK.md ",
    "TECH_STACK",
    "docs",
    "docs/",
    "../outside.md",
    "/etc/passwd",
    "https://example.com/a.md",
    "file:///tmp/a.md",
  ]) {
    assert.equal(inlineCodeFile(text), null, text);
  }
});
