import { useMemo, useState, useEffect } from "react";
import { CaretDownIcon } from "@phosphor-icons/react";
import { pageHunks, parseDiff } from "./git-diff";

export function Diff({
  text,
  ready,
  error,
  empty,
}: {
  text: string;
  ready: boolean;
  error: string;
  empty: string;
}) {
  const files = useMemo(() => parseDiff(text), [text]);
  const [fileIndex, setFileIndex] = useState(0);
  const [shown, setShown] = useState(200);
  useEffect(() => { setFileIndex(0); setShown(200); }, [text]);
  const selectedFile = files[fileIndex] ?? files[0];
  const lineCount = selectedFile?.hunks.reduce((sum, hunk) => sum + hunk.lines.length, 0) ?? 0;
  const added = files.reduce((total, file) => total + file.added, 0);
  const removed = files.reduce((total, file) => total + file.removed, 0);

  if (error) return <p className="sc-note error">{error}</p>;
  if (!ready) return <p className="sc-note">Loading diff...</p>;
  if (files.length === 0) return <p className="sc-note">{empty}</p>;

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
            <select value={fileIndex} onChange={(event) => { setFileIndex(Number(event.target.value)); setShown(200); }}>
              {files.map((file, index) => <option key={index} value={index}>{file.path}</option>)}
            </select>
            <CaretDownIcon size={10} className="diff-file-picker-caret" aria-hidden="true" />
          </span>
        </label>
      )}
      {[selectedFile].map((file) => (
        <div className="diff-file" key={`${file.path}-${fileIndex}`}>
          <div className="diff-file-head">
            <span className="diff-path">{file.path}</span>
            <span className="diff-stat">
              <span className="add">+{file.added}</span>{" "}
              <span className="del">-{file.removed}</span>
            </span>
          </div>
          {file.binary ? (
            <p className="diff-binary">Binary file</p>
          ) : file.hunks.length === 0 ? (
            <p className="diff-binary">No line changes.</p>
          ) : (
            pageHunks(file.hunks, 0, shown).map((hunk, hunkIndex) => (
              <div className="diff-hunk" key={`${hunk.header}-${hunkIndex}`}>
                <div className="diff-hunk-head">{hunk.header}</div>
                <div className="diff-lines">
                  <div className="diff-lines-inner">
                    {hunk.lines.map((line, lineIndex) => (
                      <div
                        className={`diff-line ${line.kind}`}
                        key={lineIndex}
                      >
                        <span className="diff-num">{line.oldLine ?? ""}</span>
                        <span className="diff-num">{line.newLine ?? ""}</span>
                        <span className="diff-sign">
                          {line.kind === "add"
                            ? "+"
                            : line.kind === "del"
                              ? "-"
                              : " "}
                        </span>
                        <span className="diff-text">
                          {line.text === "" ? " " : line.text}
                        </span>
                      </div>
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
