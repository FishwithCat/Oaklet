import { useEffect, useMemo, useRef } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorState, type Extension } from "@codemirror/state";
import type { EditorView } from "@uiw/react-codemirror";
import type { ChangedLine } from "../source-control/git-diff";
import { useResolvedTheme } from "../settings/theme";
import { changedLinesField, setChangedLines } from "./file-gutter";

/** Split from Files so opening the page does not load the editor bundle. */
export default function FileEditor({ path, value, syntax, readOnly, marks, onChange }: {
  path: string;
  value: string;
  syntax: Extension;
  readOnly: boolean;
  marks: readonly ChangedLine[];
  onChange: (content: string) => void;
}) {
  const theme = useResolvedTheme();
  const view = useRef<EditorView | null>(null);
  const extensions = useMemo(
    () => [syntax, EditorState.lineSeparator.of("\n"), changedLinesField],
    [syntax],
  );

  useEffect(() => {
    view.current?.dispatch({ effects: setChangedLines.of(marks) });
  }, [marks]);

  return (
    <CodeMirror
      key={path}
      className="files-editor"
      theme={theme}
      value={value}
      height="100%"
      extensions={extensions}
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
