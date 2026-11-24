import { useEffect, useImperativeHandle, useMemo, useRef, type Ref } from "react";
import CodeMirror, { EditorView, keymap } from "@uiw/react-codemirror";
import { search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import type { ChangedLine } from "../source-control/git-diff";
import { useResolvedTheme } from "../settings/theme";
import { changedLinesField, setChangedLines } from "./file-gutter";
import { FileFindPanel } from "./find-panel";

// Keep the keymap off basicSetup so the panel and its keys share one search extension.
const BASIC_SETUP = { searchKeymap: false };

/** Split from Files so opening the page does not load the editor bundle. */
export type FileEditorHandle = { jumpToLine: (line: number) => void };

export default function FileEditor({ path, value, syntax, readOnly, marks, onChange, ref }: {
  path: string;
  value: string;
  syntax: Extension;
  readOnly: boolean;
  marks: readonly ChangedLine[];
  onChange: (content: string) => void;
  ref: Ref<FileEditorHandle>;
}) {
  const theme = useResolvedTheme();
  const view = useRef<EditorView | null>(null);
  const extensions = useMemo(
    () => [
      syntax,
      EditorState.lineSeparator.of("\n"),
      changedLinesField,
      search({ createPanel: (editor) => new FileFindPanel(editor) }),
      keymap.of(searchKeymap),
    ],
    [syntax],
  );

  useEffect(() => {
    view.current?.dispatch({ effects: setChangedLines.of(marks) });
  }, [marks]);

  useImperativeHandle(ref, () => ({
    jumpToLine(line) {
      const editor = view.current;
      if (!editor || line < 1 || line > editor.state.doc.lines) return;
      const position = editor.state.doc.line(line).from;
      editor.dispatch({
        selection: { anchor: position },
        effects: EditorView.scrollIntoView(position, { y: "center" }),
      });
      editor.focus();
    },
  }), []);

  return (
    <CodeMirror
      key={path}
      className="files-editor"
      theme={theme}
      value={value}
      height="100%"
      extensions={extensions}
      basicSetup={BASIC_SETUP}
      readOnly={readOnly}
      editable={!readOnly}
      aria-label={`File contents: ${path}`}
      onCreateEditor={(editor) => {
        view.current = editor;
        // The view can be created a commit after this component's effect runs.
        editor.dispatch({ effects: setChangedLines.of(marks) });
      }}
      onChange={(_, update) => onChange(update.state.sliceDoc())}
    />
  );
}
