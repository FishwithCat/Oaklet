import assert from "node:assert/strict";
import test from "node:test";
import { quoteExcerpt } from "../src/shared/chat-quote.ts";
import { quoteDiffSelection } from "../src/features/source-control/diff-selection.ts";

test("quotes preserve whitespace and fence-like source text", () => {
  assert.equal(
    quoteExcerpt("File notes.md:2-4", "  ```\r\n\r\n  @queue  "),
    "> ````\n> File notes.md:2-4\n> \n>   ```\n> \n>   @queue  \n> ````",
  );
});

test("diff quotes track old and new paths and skip gaps between selected lines", () => {
  assert.equal(
    quoteDiffSelection("src/new.ts", "src/old.ts", [
      { oldLine: 2, newLine: 0, text: "old tail" },
      { oldLine: 0, newLine: 3, text: "new text" },
      { oldLine: 3, newLine: 4, text: "" },
      { oldLine: 9, newLine: 10, text: "next head" },
    ]),
    "> ```\n> Old src/old.ts:2-3, 9\n> New src/new.ts:3-4, 10\n> \n> -old tail\n> +new text\n>  \n>  next head\n> ```",
  );
  assert.equal(
    quoteDiffSelection("src/new.ts", null, [{ oldLine: 0, newLine: 7, text: "addition" }]),
    "> ```\n> New src/new.ts:7\n> \n> +addition\n> ```",
  );
});
