import assert from "node:assert/strict";
import test from "node:test";
import { changedLines, isImagePath, pageHunks, parseDiff } from "../src/features/source-control/git-diff.ts";

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

test("only webview-supported image extensions get an image preview", () => {
  for (const path of ["a.png", "a.PNG", "dir/b.jpg", "b.jpeg", "c.gif", "d.webp"]) {
    assert.equal(isImagePath(path), true, path);
  }
  for (const path of ["a.svg", "a.bmp", "a.txt", "png", "a.png.txt"]) {
    assert.equal(isImagePath(path), false, path);
  }
});
