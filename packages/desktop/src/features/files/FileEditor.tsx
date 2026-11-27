import { useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, type Ref } from "react";
import CodeMirror, { EditorView, keymap } from "@uiw/react-codemirror";
import { search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import { QuoteToChatButton } from "../../shared/SelectionActionButton";
import { formatLineSpan, quoteExcerpt } from "../../shared/chat-quote";
import { useSelectionAction } from "../../shared/useSelectionAction";
import type { ChangedLine } from "../source-control/git-diff";
import { useResolvedTheme } from "../settings/theme";
import { changedLinesField, setChangedLines } from "./file-gutter";
import { FileFindPanel } from "./find-panel";

// Keep the keymap off basicSetup so the panel and its keys share one search extension.
const BASIC_SETUP = { searchKeymap: false };

/** Split from Files so opening the page does not load the editor bundle. */
export type FileEditorHandle = {
  jumpToLine: (line: number) => void;
};

export default function FileEditor({ path, value, syntax, readOnly, marks, onChange, onReadyChange, onQuote, ref }: {
  path: string;
  value: string;
  syntax: Extension;
  readOnly: boolean;
  marks: readonly ChangedLine[];
  onChange: (content: string) => void;
  onReadyChange: (path: string | null) => void;
  onQuote?: (quote: string) => void;
  ref: Ref<FileEditorHandle>;
}) {
  const theme = useResolvedTheme();
  const view = useRef<EditorView | null>(null);
  const contentRef = useRef<HTMLElement>(null);
  const readSelection = useCallback(() => {
    const editor = view.current;
    if (!editor?.hasFocus) return;
    const { state, viewport } = editor;
    const ranges = state.selection.ranges.filter((range) => !range.empty);
    // A selection can extend beyond the portion CodeMirror has rendered.
    const visible = ranges.filter(({ from, to }) => from < viewport.to && to > viewport.from);
    if (!visible.length) return;
    const start = editor.coordsAtPos(Math.max(visible[0].from, viewport.from));
    const end = editor.coordsAtPos(Math.min(visible[visible.length - 1].to, viewport.to), -1);
    if (!start || !end) return;
    const bounds = editor.scrollDOM.getBoundingClientRect();
    const left = Math.max(Math.min(start.left, end.left), bounds.left);
    const right = Math.min(Math.max(start.right, end.right), bounds.right);
    const top = Math.max(start.top, bounds.top);
    const bottom = Math.min(end.bottom, bounds.bottom);
    if (right < left || bottom <= top) return;
    const text = ranges.map(({ from, to }) => {
      const lines = formatLineSpan(state.doc.lineAt(from).number, state.doc.lineAt(to - 1).number);
      return quoteExcerpt(`File ${path}:${lines}`, state.sliceDoc(from, to));
    }).join("\n\n");
    return { text, rect: { left, right, top, bottom } };
  }, [path]);
  const selection = useSelectionAction({ active: !!onQuote && !readOnly, containerRef: contentRef, readSelection });
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

  useLayoutEffect(() => () => onReadyChange(null), [onReadyChange]);

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
    <>
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
          contentRef.current = editor.contentDOM;
          onReadyChange(path);
          // The view can be created a commit after this component's effect runs.
          editor.dispatch({ effects: setChangedLines.of(marks) });
        }}
        onChange={(_, update) => onChange(update.state.sliceDoc())}
      />
      {onQuote && !readOnly && <QuoteToChatButton selection={selection} onAction={onQuote} />}
    </>
  );
}
