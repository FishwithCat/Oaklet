import assert from "node:assert/strict";
import test, { after } from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

// Load the real component through Vite so this stays a JSX-free test file.
const vite = await createServer({
  configFile: false,
  server: { middlewareMode: true },
  appType: "custom",
  logLevel: "silent",
});
const { MarkdownText } = await vite.ssrLoadModule("/src/shared/Markdown.tsx");
after(() => vite.close());

const render = (text, onOpenFile) =>
  renderToStaticMarkup(React.createElement(MarkdownText, { text, onOpenFile }));

test("inline code that names a file renders as a file link", () => {
  const html = render("`TECH_STACK.md`", () => {});

  assert.match(html, /<a href="TECH_STACK\.md" class="file-link"/);
});

test("a fenced block stays code even when its content is a file name", () => {
  const html = render("```\nTECH_STACK.md\n```", () => {});

  assert.doesNotMatch(html, /class="file-link"/);
});

test("a markdown link around inline code keeps one anchor", () => {
  const html = render("[`TECH_STACK.md`](TECH_STACK.md)", () => {});

  assert.equal(html.match(/<a /g)?.length, 1);
});

test("inline code stays plain without an open handler", () => {
  assert.doesNotMatch(render("`TECH_STACK.md`"), /<a /);
});
