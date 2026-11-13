import { useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorState, type Extension } from "@codemirror/state";

/** Split from Files so opening the page does not load the editor bundle. */
export default function FileEditor({ path, value, syntax, readOnly, onChange }: {
  path: string;
  value: string;
  syntax: Extension;
  readOnly: boolean;
  onChange: (content: string) => void;
}) {
  const extensions = useMemo(
    () => [syntax, EditorState.lineSeparator.of("\n")],
    [syntax],
  );

  return (
    <CodeMirror
      key={path}
      className="files-editor"
      value={value}
      height="100%"
      extensions={extensions}
      readOnly={readOnly}
      editable={!readOnly}
      aria-label={`File contents: ${path}`}
      onChange={(_, update) => onChange(update.state.sliceDoc())}
    />
  );
}
