import { useMemo, useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { DownSmallRegular } from "@mingcute/react/core-regular";
import { DiffLineView } from "../../shared/DiffLineView";
import { ImageLightbox } from "../../shared/ImageLightbox";
import { errorText } from "../../shared/error-text";
import type { SourceSymbol } from "../../shared/source-symbol";
import { affectedSymbols, isImagePath, pageHunks, parseDiff, type DiffFile } from "./git-diff";

type DiffSymbols = { old: SourceSymbol[]; new: SourceSymbol[] };

function ImageDiff({ workspace, path, commit, revision }: { workspace: string; path: string; commit: string | null; revision: number }) {
  const [src, setSrc] = useState("");
  const [error, setError] = useState("");
  const [enlarged, setEnlarged] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setSrc("");
    setError("");
    const request = commit
      ? invoke<string>("git_show_image", { hash: commit, path })
      : invoke<string>("files_read_image", { workspace, path });
    request
      .then((data) => { if (!cancelled) setSrc(data); })
      .catch((err) => { if (!cancelled) setError(errorText(err)); });
    return () => { cancelled = true; };
  }, [workspace, path, commit, revision]);

  if (error) return <p className="diff-binary diff-image-error">{error}</p>;
  if (!src) return <p className="diff-binary">Loading image...</p>;
  return (
    <div className="diff-image">
      <button type="button" className="diff-image-button" title="Enlarge image" aria-label={`Enlarge ${path}`} onClick={() => setEnlarged(true)}>
        <img src={src} alt={path} />
      </button>
      {enlarged && <ImageLightbox src={src} alt={path} onClose={() => setEnlarged(false)} />}
    </div>
  );
}

export function Diff({
  text,
  ready,
  error,
  empty,
  workspace,
  commit,
  change,
  revision,
}: {
  text: string;
  ready: boolean;
  error: string;
  empty: string;
  workspace: string;
  commit: string | null;
  change?: string | null;
  revision: number;
}) {
  const files = useMemo(() => parseDiff(text), [text]);
  const [fileIndex, setFileIndex] = useState(0);
  const [shown, setShown] = useState(200);
  const [symbolResult, setSymbolResult] = useState<{
    file: DiffFile; workspace: string; commit: string | null; change?: string | null;
    revision: number; symbols: DiffSymbols;
  } | null>(null);
  const [jump, setJump] = useState<{ file: DiffFile; hunk: number } | null>(null);
  const diffFile = useRef<HTMLDivElement>(null);
  useEffect(() => { setFileIndex(0); setShown(200); }, [text]);
  const selectedIndex = files[fileIndex] ? fileIndex : 0;
  const selectedFile: DiffFile | undefined = files[selectedIndex];
  const lineCount = selectedFile?.hunks.reduce((sum, hunk) => sum + hunk.lines.length, 0) ?? 0;
  const added = files.reduce((total, file) => total + file.added, 0);
  const removed = files.reduce((total, file) => total + file.removed, 0);
  const symbols = symbolResult && symbolResult.file === selectedFile && symbolResult.workspace === workspace &&
    symbolResult.commit === commit && symbolResult.change === change && symbolResult.revision === revision
    ? symbolResult.symbols : null;
  const hunkSymbols = useMemo(() => selectedFile && symbols
    ? affectedSymbols(selectedFile.hunks, symbols.old, symbols.new)
    : [], [selectedFile, symbols]);
  const symbolHunks = useMemo(() => {
    const locations = new Map<string, number>();
    hunkSymbols.forEach((names, hunk) => {
      names.forEach((name) => {
        if (!locations.has(name)) locations.set(name, hunk);
      });
    });
    return locations;
  }, [hunkSymbols]);

  useEffect(() => {
    if (!ready || !selectedFile?.indexLine || !selectedFile.hasSourceOutline ||
        selectedFile.binary || !selectedFile.hunks.length || (!commit && !change)) return;
    let cancelled = false;
    invoke<DiffSymbols | null>("git_diff_symbols", {
      path: commit ? null : change, hash: commit, fileIndex: selectedIndex,
      fileHeader: selectedFile.header, indexLine: selectedFile.indexLine,
    }).then((result) => {
      if (!cancelled && result) {
        setSymbolResult({ file: selectedFile, workspace, commit, change, revision, symbols: result });
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [ready, selectedFile, selectedIndex, workspace, commit, change, revision]);

  useEffect(() => {
    if (!jump || jump.file !== selectedFile) return;
    const header = diffFile.current?.querySelector<HTMLElement>(`[data-hunk="${jump.hunk}"]`);
    header?.scrollIntoView({ block: "start" });
    header?.focus({ preventScroll: true });
  }, [jump, selectedFile]);

  function jumpToHunk(file: DiffFile, hunk: number) {
    const start = file.hunks.slice(0, hunk).reduce((count, entry) => count + entry.lines.length, 0);
    setShown((current) => Math.max(current, start + 200));
    setJump({ file, hunk });
  }

  if (error) return <p className="sc-note error">{error}</p>;
  if (!ready) return <p className="sc-note">Loading diff...</p>;
  if (!selectedFile) return <p className="sc-note">{empty}</p>;

  return (
    <div className="diff">
      <div className="diff-summary">
        {files.length} file{files.length === 1 ? "" : "s"} changed
        <span className="diff-stat">
          <span className="add">+{added}</span>{" "}
          <span className="del">-{removed}</span>
        </span>
      </div>
      {files.length > 1 && (
        <label className="diff-file-picker">
          <span className="diff-file-picker-label">File</span>
          <span className="diff-file-picker-control">
            <select value={selectedIndex} onChange={(event) => { setFileIndex(Number(event.target.value)); setShown(200); }}>
              {files.map((file, index) => <option key={index} value={index}>{file.path}</option>)}
            </select>
            <DownSmallRegular size={10} className="diff-file-picker-caret" aria-hidden="true" />
          </span>
        </label>
      )}
      {[selectedFile].map((file) => (
        <div className="diff-file" key={`${file.path}-${selectedIndex}`} ref={diffFile}>
          <div className="diff-file-head">
            <span className="diff-path">{file.path}</span>
            <span className="diff-stat">
              <span className="add">+{file.added}</span>{" "}
              <span className="del">-{file.removed}</span>
            </span>
          </div>
          {symbolHunks.size > 0 && (
            <nav className="diff-symbols" aria-label="Changed symbols">
              {[...symbolHunks].map(([name, hunk]) => (
                <button key={name} type="button" onClick={() => jumpToHunk(file, hunk)} title={name}>{name}</button>
              ))}
            </nav>
          )}
          {file.binary ? (
            isImagePath(file.path) ? (
              <ImageDiff workspace={workspace} path={file.path} commit={commit} revision={revision} />
            ) : (
              <p className="diff-binary">Binary file</p>
            )
          ) : file.hunks.length === 0 ? (
            <p className="diff-binary">No line changes.</p>
          ) : (
            pageHunks(file.hunks, 0, shown).map((hunk, hunkIndex) => (
              <div className="diff-hunk" key={`${hunk.header}-${hunkIndex}`}>
                <div className="diff-hunk-head" data-hunk={hunkIndex} tabIndex={-1}>
                  {hunk.header}
                  {hunkSymbols[hunkIndex]?.length > 0 && <span className="diff-hunk-symbols">{hunkSymbols[hunkIndex].join(", ")}</span>}
                </div>
                <div className="diff-lines">
                  <div className="diff-lines-inner">
                    {hunk.lines.map((line, lineIndex) => (
                      <DiffLineView
                        key={lineIndex}
                        line={line}
                        path={file.path}
                        numbers
                      />
                    ))}
                  </div>
                </div>
              </div>
            ))
          )}
          {!file.binary && shown < lineCount && (
            <button type="button" className="diff-more" onClick={() => setShown((current) => current + 200)}>
              <span>Show more lines</span>
              <span className="diff-more-count">{shown} of {lineCount}</span>
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
