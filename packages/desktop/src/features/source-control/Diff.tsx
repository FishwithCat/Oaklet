import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ArrowDownRegular, ArrowUpRegular } from "@mingcute/react/core-regular";
import { DiffLineView } from "../../shared/DiffLineView";
import { ImageLightbox } from "../../shared/ImageLightbox";
import { errorText } from "../../shared/error-text";
import type { SourceSymbol } from "../../shared/source-symbol";
import {
  affectedSymbols, contextGap, hunkOffsets, isImagePath, pageHunks, parseDiff,
  type ContextRange, type DiffFile, type DiffLine,
} from "./git-diff";
import { statusInfo } from "./git-model";
import type { ReviewTarget } from "./useSourceControl";

const PAGE_LINES = 200;
const CONTEXT_LINES = 40;

export type DiffEdge = "first" | "last";

type Snapshot = { fileHeader: string; indexLine: string };
type ReviewDetails = {
  oldLines: number;
  newLines: number;
  symbols: { old: SourceSymbol[]; new: SourceSymbol[] };
};
type GapState = { lines: DiffLine[]; pending: boolean; error: string };

type Navigation = {
  previous: (() => void) | undefined;
  next: (() => void) | undefined;
  previousFile: boolean;
  nextFile: boolean;
};

function readSnapshot(file: DiffFile | undefined): Snapshot | null {
  return file?.indexLine ? { fileHeader: file.header, indexLine: file.indexLine } : null;
}

function ImageDiff({ target, file }: { target: ReviewTarget; file: DiffFile }) {
  const [src, setSrc] = useState("");
  const [error, setError] = useState("");
  const [enlarged, setEnlarged] = useState(false);
  const { path } = target;

  useEffect(() => {
    const snapshot = readSnapshot(file);
    if (!snapshot) return;
    let cancelled = false;
    invoke<string>("git_review_image", { target, snapshot, workspace: target.workspace })
      .then((data) => { if (!cancelled) setSrc(data); })
      .catch((err) => { if (!cancelled) setError(errorText(err)); });
    return () => { cancelled = true; };
  }, [target, file]);

  if (error) return <p className="diff-binary diff-image-error" role="status">{error}</p>;
  if (!file.indexLine) return <p className="diff-binary">Image preview unavailable.</p>;
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

function DiffBar({ target, navigation, current, count, added, removed }: {
  target: ReviewTarget;
  navigation: Navigation;
  current?: number;
  count?: number;
  added?: number;
  removed?: number;
}) {
  const { previous, next, previousFile, nextFile } = navigation;
  const info = statusInfo(target.status);
  return (
    <header className="diff-file-head">
      <span className={`change-status ${target.status}`} title={info.title} aria-label={info.title}>{info.label}</span>
      <div className="diff-path">
        <span title={target.path}>{target.path}</span>
        {target.oldPath && <span className="change-previous" title={target.oldPath}>From {target.oldPath}</span>}
      </div>
      {added !== undefined && removed !== undefined && (added > 0 || removed > 0) && (
        <span className="diff-stat"><span className="add">+{added}</span><span className="del">−{removed}</span></span>
      )}
      <nav className="diff-navigation" aria-label="Change navigation">
        <button type="button" className="ghost icon-button" disabled={!previous} onClick={previous}
          aria-label={previousFile ? "Previous changed file" : "Previous change"}
          title={previousFile ? "Previous changed file (⌥↑)" : "Previous change (⌥↑)"}
          aria-keyshortcuts="Alt+ArrowUp"
        ><ArrowUpRegular size={14} /></button>
        {!!count && <span className="diff-position" aria-label={`Change ${(current ?? 0) + 1} of ${count}`}>{(current ?? 0) + 1}/{count}</span>}
        <button type="button" className="ghost icon-button" disabled={!next} onClick={next}
          aria-label={nextFile ? "Next changed file" : "Next change"}
          title={nextFile ? "Next changed file (⌥↓)" : "Next change (⌥↓)"}
          aria-keyshortcuts="Alt+ArrowDown"
        ><ArrowDownRegular size={14} /></button>
      </nav>
    </header>
  );
}

function navigate(event: KeyboardEvent, { previous, next }: Navigation) {
  if (!event.altKey || event.metaKey || event.ctrlKey || event.shiftKey) return;
  const action = event.key === "ArrowUp" ? previous : event.key === "ArrowDown" ? next : undefined;
  if (action) {
    event.preventDefault();
    event.stopPropagation();
    action();
  }
}

function DiffRows({ lines, path }: { lines: readonly DiffLine[]; path: string }) {
  return (
    <div className="diff-lines">
      <div className="diff-lines-inner">
        {lines.map((line, index) => (
          <DiffLineView key={`${line.oldLine}:${line.newLine}:${line.kind === "meta" ? index : ""}`} line={line} path={path} numbers />
        ))}
      </div>
    </div>
  );
}

function Gap({ range, state, tail, onExpand, path }: {
  range: ContextRange;
  state: GapState | undefined;
  tail: boolean;
  onExpand: () => void;
  path: string;
}) {
  const shown = state?.lines.length ?? 0;
  const left = range.count - shown;
  const rows = state && shown > 0 && <DiffRows lines={state.lines} path={path} />;
  return (
    <div className="diff-gap">
      {tail && rows}
      <button type="button" className="diff-more" aria-disabled={state?.pending || left === 0} onClick={onExpand}>
        {state?.pending ? "Loading context..." : left === 0 ? "Unchanged context" : "Show context"}
        {left > 0 && <span className="diff-more-count">{left} hidden {left === 1 ? "line" : "lines"}</span>}
      </button>
      {state?.error && <p className="diff-binary diff-image-error" role="status">{state.error}</p>}
      {!tail && rows}
    </div>
  );
}

type DiffProps = {
  target: ReviewTarget;
  text: string | null;
  error: string;
  landing: DiffEdge | null;
  onPreviousFile?: () => void;
  onNextFile?: () => void;
};

export function Diff(props: DiffProps) {
  const { target, text, error, onPreviousFile, onNextFile, landing } = props;
  const files = useMemo(() => text ? parseDiff(text) : [], [text]);
  if (files.length > 0) return <FileDiff {...props} files={files} />;
  const navigation = { previous: onPreviousFile, next: onNextFile, previousFile: true, nextFile: true };
  return (
    <div className="diff" onKeyDown={(event) => navigate(event, navigation)}>
      <DiffBar target={target} navigation={navigation} />
      <p className={`sc-note${error ? " error" : ""}`} role="status" tabIndex={0}
        ref={(element) => {
          if (landing && (error || text !== null) && element?.closest(".sc-detail") === document.activeElement) {
            element?.focus({ preventScroll: true });
          }
        }}
      >
        {error || (text === null ? "Loading diff..." : "No diff for this file.")}
      </p>
    </div>
  );
}

function FileDiff({ target, files, landing, onPreviousFile, onNextFile }: DiffProps & { files: DiffFile[] }) {
  const hunks = useMemo(() => files.flatMap((file) => file.hunks), [files]);
  const { offsets, lineCount } = useMemo(() => hunkOffsets(hunks), [hunks]);
  const file = files.length === 1 ? files[0] : undefined;
  const binary = files.find((entry) => entry.binary);
  const [entry] = useState(landing);
  const initialHunk = entry === "last" ? Math.max(0, hunks.length - 1) : 0;
  const initialOffset = offsets[initialHunk] ?? 0;
  const [current, setCurrent] = useState(initialHunk);
  const [{ offset, end }, setPage] = useState({ offset: initialOffset, end: initialOffset + PAGE_LINES });
  const [jump, setJump] = useState<{ hunk: number | null } | null>(null);
  const [details, setDetails] = useState<ReviewDetails | null>(null);
  const [detailsError, setDetailsError] = useState("");
  const [gaps, setGaps] = useState<Record<number, GapState>>({});
  const scroll = useRef<HTMLDivElement>(null);
  const page = useMemo(() => pageHunks(hunks, offset, end - offset), [hunks, offset, end]);
  const snapshot = useMemo(() => readSnapshot(file), [file]);

  useEffect(() => {
    if (!snapshot || !hunks.length) return;
    let cancelled = false;
    invoke<ReviewDetails>("git_review_details", { target, snapshot, workspace: target.workspace })
      .then((data) => { if (!cancelled) setDetails(data); })
      .catch((error) => { if (!cancelled) setDetailsError(errorText(error)); });
    return () => { cancelled = true; };
  }, [target, snapshot, hunks.length]);

  const hunkSymbols = useMemo(() => details
    ? affectedSymbols(hunks, details.symbols.old, details.symbols.new)
    : [], [hunks, details]);

  useLayoutEffect(() => {
    const viewport = scroll.current;
    if (!viewport) return;
    const entering = entry && viewport.closest(".sc-detail") === document.activeElement;
    if (!jump && !entering) return;
    const hunk = jump ? jump.hunk : initialHunk;
    const header = hunk === null ? null : viewport.querySelector<HTMLElement>(`[data-hunk="${hunk}"]`);
    if (header) {
      viewport.scrollTop += header.getBoundingClientRect().top - viewport.getBoundingClientRect().top;
      header.focus({ preventScroll: true });
    } else {
      viewport.scrollTop = 0;
      viewport.focus({ preventScroll: true });
    }
  }, [entry, initialHunk, jump]);

  function jumpTo(hunk: number) {
    setCurrent(hunk);
    setPage({ offset: offsets[hunk], end: offsets[hunk] + PAGE_LINES });
    setJump({ hunk });
  }

  function goToPage(offset: number, end: number) {
    setPage({ offset, end });
    setCurrent(pageHunks(hunks, offset, 1)[0]?.index ?? 0);
    setJump({ hunk: null });
  }

  function followScroll() {
    const viewport = scroll.current;
    if (!viewport || !page.length) return;
    const top = viewport.getBoundingClientRect().top;
    let hunk = page[0].index;
    for (const heading of viewport.querySelectorAll<HTMLElement>("[data-hunk]")) {
      if (heading.getBoundingClientRect().top > top + 1) break;
      hunk = Number(heading.dataset.hunk);
    }
    setCurrent(hunk);
  }

  async function expand(index: number, range: ContextRange, tail: boolean) {
    const state = gaps[index];
    const lines = state?.lines ?? [];
    const left = range.count - lines.length;
    if (!snapshot || state?.pending || left <= 0) return;
    const count = Math.min(CONTEXT_LINES, left);
    const delta = tail ? lines.length : left - count;
    const oldStart = range.oldStart + delta;
    const newStart = range.newStart + delta;
    setGaps((current) => ({ ...current, [index]: { lines, pending: true, error: "" } }));
    try {
      const rows = await invoke<string[]>("git_review_context", {
        workspace: target.workspace,
        target, snapshot, range: { oldStart, newStart, count },
      });
      const context = rows.map((text, row): DiffLine => ({
        kind: "context", text, oldLine: oldStart + row, newLine: newStart + row,
      }));
      setGaps((current) => ({
        ...current,
        [index]: { lines: tail ? [...lines, ...context] : [...context, ...lines], pending: false, error: "" },
      }));
    } catch (error) {
      setGaps((current) => ({ ...current, [index]: { lines, pending: false, error: errorText(error) } }));
    }
  }

  function gap(index: number) {
    if (!details) return null;
    const range = contextGap(hunks[index - 1], hunks[index], details.oldLines, details.newLines);
    const tail = index === hunks.length;
    return range && <Gap range={range} state={gaps[index]} tail={tail} path={target.path} onExpand={() => void expand(index, range, tail)} />;
  }

  const previousFile = current === 0;
  const nextFile = current + 1 >= hunks.length;
  const navigation = {
    previous: previousFile ? onPreviousFile : () => jumpTo(current - 1),
    next: nextFile ? onNextFile : () => jumpTo(current + 1),
    previousFile,
    nextFile,
  };

  return (
    <div className="diff" onKeyDown={(event) => navigate(event, navigation)}>
      <DiffBar target={target} navigation={navigation} current={current} count={hunks.length}
        added={files.reduce((total, entry) => total + entry.added, 0)}
        removed={files.reduce((total, entry) => total + entry.removed, 0)}
      />
      <div className="diff-scroll" ref={scroll} tabIndex={0} aria-label="File diff" onScroll={followScroll}>
        {detailsError && <p className="diff-binary" role="status">{detailsError}</p>}
        {offset > 0 && (
          <button type="button" className="diff-more" onClick={() => goToPage(Math.max(0, offset - PAGE_LINES), offset)}>
            Show previous diff lines
          </button>
        )}
        {page.map((hunk) => (
          <Fragment key={hunk.index}>
            {hunk.offset === 0 && gap(hunk.index)}
            <div className="diff-hunk">
              <div className="diff-hunk-head" data-hunk={hunk.index} tabIndex={-1}>
                {hunk.header}
                {hunkSymbols[hunk.index]?.length > 0 && <span className="diff-hunk-symbols">{hunkSymbols[hunk.index].join(", ")}</span>}
              </div>
              <DiffRows lines={hunk.lines} path={target.path} />
            </div>
          </Fragment>
        ))}
        {end < lineCount ? (
          <button type="button" className="diff-more" onClick={() => goToPage(end, end + PAGE_LINES)}>
            Show next diff lines <span className="diff-more-count">{end} of {lineCount}</span>
          </button>
        ) : hunks.length > 0 && gap(hunks.length)}
        {binary && (
          isImagePath(target.path) || (target.oldPath && isImagePath(target.oldPath)) ? (
            <ImageDiff target={target} file={binary} />
          ) : <p className="diff-binary">Binary file</p>
        )}
        {!binary && hunks.length === 0 && <p className="diff-binary">File metadata changed. No line changes.</p>}
      </div>
    </div>
  );
}
