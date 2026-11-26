import assert from "node:assert/strict";
import test from "node:test";
import { fileLanguage } from "../src/features/files/file-language.ts";

test("file editor parses the languages supported by the source outline", async () => {
  const samples = [
    [["js", "mjs", "cjs"], "const answer = 42;"],
    [["jsx"], "const view = <main />;"],
    [["ts", "mts", "cts"], "const answer: number = 42;"],
    [["tsx"], "const view = (title: string) => <main>{title}</main>;"],
    [["rs"], "fn answer() -> i32 { 42 }"],
    [["py"], "def answer():\n    return 42\n"],
    [["java"], "class Example { int answer() { return 42; } }"],
  ];

  for (const [extensions, source] of samples) {
    for (const extension of extensions) {
      const path = `src/example.${extension}`;
      const syntax = await fileLanguage(path);
      assert.ok(syntax, path);
      syntax.language.parser.parse(source).iterate({
        enter(node) { assert.equal(node.type.isError, false, path); },
      });
    }
  }
});
