import assert from "node:assert/strict";
import test from "node:test";
import { affectedSymbols, changedLines, contextGap, hunkOffsets, isImagePath, pageHunks, parseDiff } from "../src/features/source-control/git-diff.ts";

import { chooseReviewPath, filterReviewFiles, sortReviewFiles } from "../src/features/source-control/git-model.ts";

const PATCH = `diff --git a/src/a.ts b/src/a.ts
index 1111111..2222222 100644
--- a/src/a.ts
+++ b/src/a.ts
@@ -1,3 +1,4 @@
 const a = 1;
-const b = 2;
+const b = 3;
+const c = 4;
 export { a };
diff --git a/README.md b/README.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/README.md
@@ -0,0 +1,2 @@
+# Oaklet
+hello
`;

test("parseDiff groups files and tracks line numbers", () => {
  const files = parseDiff(PATCH);

  assert.equal(files.length, 2);
  assert.equal(files[0].path, "src/a.ts");
  assert.equal(files[0].added, 2);
  assert.equal(files[0].removed, 1);
  assert.deepEqual(
    files[0].hunks[0].lines.map((line) => [
      line.kind,
      line.text,
      line.oldLine,
      line.newLine,
    ]),
    [
      ["context", "const a = 1;", 1, 1],
      ["del", "const b = 2;", 2, null],
      ["add", "const b = 3;", null, 2],
      ["add", "const c = 4;", null, 3],
      ["context", "export { a };", 3, 4],
    ],
  );

  assert.equal(files[1].path, "README.md");
  assert.equal(files[1].added, 2);
  assert.equal(files[1].removed, 0);
});

test("diff pages preserve line numbers across hunk boundaries without dropping lines", () => {
  const hunks = parseDiff(PATCH).flatMap((file) => file.hunks);
  const lines = hunks.flatMap((hunk) => hunk.lines);
  const pages = [0, 3, 6].flatMap((offset) => pageHunks(hunks, offset, 3).flatMap((hunk) => hunk.lines));
  assert.deepEqual(pages, lines);
  assert.deepEqual(pageHunks(hunks, 99, 3), []);
  assert.equal(pageHunks(hunks, 3, 3).length, 2);
  assert.equal(pageHunks(hunks, 3, 3)[1].header, hunks[1].header);
});

test("gutter marks read a replaced block as modified and an insertion as added", () => {
  assert.deepEqual(changedLines(parseDiff(PATCH)), [
    { line: 2, kind: "modified" },
    { line: 3, kind: "modified" },
    { line: 1, kind: "added" },
    { line: 2, kind: "added" },
  ]);

  const insertion = `diff --git a/x.txt b/x.txt
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,3 @@
 one
+two
 three
`;
  assert.deepEqual(changedLines(parseDiff(insertion)), [{ line: 2, kind: "added" }]);
});

test("diff symbols use the smallest declaration on each side and ignore context", () => {
  const { hunks, indexLine } = parseDiff(PATCH)[0];
  const symbol = (name, startLine, endLine, container = "") =>
    ({ name, startLine, endLine, container, kind: "function" });
  const old = [symbol("container", 1, 3), symbol("previous", 2, 2, "container")];
  const next = [symbol("container", 1, 4), symbol("updated", 2, 2, "container"),
    symbol("inserted", 3, 3, "container"), symbol("unchanged", 4, 4, "container")];
  assert.equal(indexLine, "1111111..2222222 100644");
  assert.deepEqual(affectedSymbols(hunks, old, next), [
    ["container / previous", "container / updated", "container / inserted"],
  ]);
});

test("diffs enable source outlines for Python and Java files", () => {
  for (const path of ["script.py", "Service.java"]) {
    const patch = `diff --git a/${path} b/${path}
--- a/${path}
+++ b/${path}
@@ -1 +1 @@
-before
+after
`;
    assert.equal(parseDiff(patch)[0].hasSourceOutline, true, path);
  }
});

test("only webview-supported image extensions get an image preview", () => {
  for (const path of ["a.png", "a.PNG", "dir/b.jpg", "b.jpeg", "c.gif", "d.webp"]) {
    assert.equal(isImagePath(path), true, path);
  }
  for (const path of ["a.svg", "a.bmp", "a.txt", "png", "a.png.txt"]) {
    assert.equal(isImagePath(path), false, path);
  }
});

test("a late hunk can be rendered by its offset without rendering the preceding pages", () => {
  const lines = Array.from({ length: 260 }, (_, i) => `+row ${i + 1}`).join("\n");
  const [file] = parseDiff(`diff --git a/large.txt b/large.txt
@@ -0,0 +1,260 @@
${lines}
@@ -480 +740 @@
-old value
+new value
`);
  const { offsets, lineCount } = hunkOffsets(file.hunks);
  assert.deepEqual(offsets, [0, 260]);
  assert.equal(lineCount, 262);
  const [hunk] = pageHunks(file.hunks, offsets[1], 200);
  assert.equal(hunk.index, 1);
  assert.equal(hunk.offset, 0);
  assert.equal(hunk.lines[1].newLine, 740);
  assert.deepEqual(pageHunks(file.hunks, 60, 200)[0].lines, file.hunks[0].lines.slice(60));
});

test("context gaps preserve both sides' line numbers across insertions and file boundaries", () => {
  const [file] = parseDiff(`diff --git a/x.txt b/x.txt
@@ -2,0 +3 @@
+insertion
@@ -100,2 +101,2 @@
-old
+new
 stable
`);
  const [first, last] = file.hunks;
  assert.deepEqual(contextGap(undefined, first, 130, 131), { oldStart: 1, newStart: 1, count: 2 });
  assert.deepEqual(contextGap(first, last, 130, 131), { oldStart: 3, newStart: 4, count: 97 });
  assert.deepEqual(contextGap(last, undefined, 130, 131), { oldStart: 102, newStart: 103, count: 29 });
  assert.equal(contextGap(last, undefined, 101, 102), null);
});

test("review files sort by path, match rename sources, and retain or advance selection on refresh", () => {
  const files = sortReviewFiles([
    { path: "src/结果.ts", status: "modified" },
    { path: "README.md", status: "modified" },
    { path: "docs/new name.md", oldPath: "docs/Old name.md", status: "renamed" },
  ]);
  assert.deepEqual(files.map((file) => file.path), ["README.md", "docs/new name.md", "src/结果.ts"]);
  assert.deepEqual(filterReviewFiles(files, "OLD NAME"), [files[1]]);
  assert.equal(chooseReviewPath(files, files[1].path), files[1].path);
  assert.equal(chooseReviewPath([files[0], files[2]], files[1].path), files[2].path);
  assert.equal(chooseReviewPath(files.slice(0, 2), files[2].path), files[1].path);
  assert.equal(chooseReviewPath([], files[2].path), null);
});
