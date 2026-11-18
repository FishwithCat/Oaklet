import assert from "node:assert/strict";
import test from "node:test";
import { EditorState } from "@codemirror/state";
import { changedLinesField, setChangedLines } from "../src/features/files/file-gutter.ts";

function marksOf(state) {
  const marks = [];
  for (const iter = state.field(changedLinesField).iter(0); iter.value; iter.next()) {
    marks.push([iter.from, iter.value.elementClass]);
  }
  return marks;
}

test("gutter marks cover the requested lines and follow inserts above them", () => {
  let state = EditorState.create({ doc: "a\nb\nc\nd", extensions: [changedLinesField] });
  assert.deepEqual(marksOf(state), []);

  state = state.update({
    effects: setChangedLines.of([
      { line: 2, kind: "added" },
      { line: 4, kind: "modified" },
      { line: 99, kind: "added" },
    ]),
  }).state;
  assert.deepEqual(marksOf(state), [[2, "cm-line-added"], [6, "cm-line-modified"]]);

  state = state.update({ changes: { from: 0, insert: "z\n" } }).state;
  assert.deepEqual(marksOf(state), [[4, "cm-line-added"], [8, "cm-line-modified"]]);
});

test("a mark stays at the line start when text is typed there", () => {
  let state = EditorState.create({ doc: "a\nb\nc", extensions: [changedLinesField] });
  state = state.update({ effects: setChangedLines.of([{ line: 2, kind: "added" }]) }).state;
  state = state.update({ changes: { from: 2, insert: "x" } }).state;
  assert.deepEqual(marksOf(state), [[2, "cm-line-added"]]);
});
