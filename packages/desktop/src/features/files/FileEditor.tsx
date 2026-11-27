import { useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, type Ref } from "react";
import CodeMirror, { EditorView, keymap } from "@uiw/react-codemirror";
import { search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import { formatLineSpan, quoteExcerpt } from "../../shared/chat-quote";
import type { ChangedLine } from "../source-control/git-diff";
import { useResolvedTheme } from "../settings/theme";
import { changedLinesField, setChangedLines } from "./file-gutter";
import { FileFindPanel } from "./find-panel";

// Keep the keymap off basicSetup so the panel and its keys share one search extension.
const BASIC_SETUP = { searchKeymap: false };

/** Split from Files so opening the page does not load the editor bundle. */
export type FileEditorHandle = {
  jumpToLine: (line: number) => void;
  quoteSelection: () => string;
};

export default function FileEditor({ path, value, syntax, readOnly, marks, onChange, onReadyChange, onSelectionChange, ref }: {
  path: string;
  value: string;
  syntax: Extension;
  readOnly: boolean;
  marks: readonly ChangedLine[];
  onChange: (content: string) => void;
  onReadyChange: (path: string | null) => void;
  onSelectionChange: (selected: boolean) => void;
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
      EditorView.updateListener.of((update) => {
        if (update.selectionSet || update.docChanged) {
          onSelectionChange(update.state.selection.ranges.some((range) => !range.empty));
        }
      }),
    ],
    [syntax, onSelectionChange],
  );

  useEffect(() => {
    view.current?.dispatch({ effects: setChangedLines.of(marks) });
  }, [marks]);

  useLayoutEffect(() => () => {
    onReadyChange(null);
    onSelectionChange(false);
  }, [onReadyChange, onSelectionChange]);

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
    quoteSelection() {
      const state = view.current?.state;
      if (!state) return "";
      return state.selection.ranges.filter((range) => !range.empty).map(({ from, to }) => {
        const lines = formatLineSpan(state.doc.lineAt(from).number, state.doc.lineAt(to - 1).number);
        return quoteExcerpt(`File ${path}:${lines}`, state.sliceDoc(from, to));
      }).join("\n\n");
    },
  }), [path]);

  return (
    <CodeMirror
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
        onReadyChange(path);
        // The view can be created a commit after this component's effect runs.
        editor.dispatch({ effects: setChangedLines.of(marks) });
      }}
      onChange={(_, update) => onChange(update.state.sliceDoc())}
    />
  );
}
