import assert from "node:assert/strict";
import test from "node:test";
import { parseDiff } from "../src/git-diff.ts";

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
