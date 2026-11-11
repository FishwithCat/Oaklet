import test from "node:test";
import assert from "node:assert/strict";
import { breadcrumbParts, fileTreeRows, lineSeparator, editorText, savedText } from "../src/features/files/file-model.ts";

test("breadcrumbs start at the workspace root and split both separators", () => {
  assert.deepEqual(breadcrumbParts("/Users/me/acorn", "src/styles/theme.css"), [
    "acorn",
    "src",
    "styles",
    "theme.css",
  ]);
  assert.deepEqual(breadcrumbParts("C:\\work\\acorn\\", "src\\main.ts"), [
    "acorn",
    "src",
    "main.ts",
  ]);
});

test("tree reveals only expanded branches and preserves nested expansion", () => {
  const folder = (path) => ({ path, name: path.split("/").pop(), directory: true });
  const file = (path) => ({ path, name: path.split("/").pop(), directory: false });
  const directories = {
    ".": [folder("src"), file("README.md")],
    src: [folder("src/features"), file("src/main.ts")],
    "src/features": [file("src/features/chat.ts")],
  };
  const expanded = new Set([".", "src", "src/features"]);
  assert.deepEqual(fileTreeRows(directories, expanded).map(({ entry, depth, parent }) => [entry.path, depth, parent]), [
    ["src", 0, "."], ["src/features", 1, "src"],
    ["src/features/chat.ts", 2, "src/features"], ["src/main.ts", 1, "src"], ["README.md", 0, "."],
  ]);
  assert.equal(fileTreeRows(directories, expanded)[3].position, 2);
  assert.equal(fileTreeRows(directories, expanded)[3].siblings, 2);
  expanded.delete(".");
  assert.deepEqual(fileTreeRows(directories, expanded), []);
  expanded.add(".");
  assert.equal(fileTreeRows(directories, expanded).length, 5);
  expanded.delete("src");
  assert.deepEqual(fileTreeRows(directories, expanded).map(({ entry }) => entry.path), ["src", "README.md"]);
  expanded.add("src");
  assert.equal(fileTreeRows(directories, expanded)[2].entry.path, "src/features/chat.ts");
  assert.equal(fileTreeRows({ ...directories, ".": [] }, expanded).length, 0);
  assert.equal(fileTreeRows({ ".": [folder("new")] }, new Set([".", "new"])).length, 1);
  assert.deepEqual(fileTreeRows({}, expanded), []);
});

test("preserves CRLF and keeps mixed endings literal", () => {
  assert.equal(lineSeparator("a\r\nb\r\n"), "\r\n");
  assert.equal(lineSeparator("a\nb\n"), "\n");
  assert.equal(lineSeparator("a\r\nb\n"), "\n");
  assert.equal(lineSeparator(""), "\n");
});

// Exercise the editor's actual text representation, including mixed line endings.
test("editing preserves existing line endings and Unicode", async () => {
  const { EditorState } = await import("@codemirror/state");
  for (const original of ["one\r\ntwo\r\n", "one\ntwo\n", "one\r\ntwo\n", "one\rtwo", ""]) {
    const state = EditorState.create({ doc: editorText(original), extensions: EditorState.lineSeparator.of("\n") });
    assert.equal(savedText(state.doc.toString(), original), original);
    const edited = state.update({ changes: { from: 0, to: 0, insert: "你好" } }).state;
    assert.equal(savedText(edited.doc.toString(), original), "你好" + original);
  }
});
