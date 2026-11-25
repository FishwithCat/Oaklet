import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { CheckRegular, CopyRegular, InformationRegular } from "@mingcute/react/core-regular";
import { fullDate, relativeDate, type CommitInfo } from "./git-model";

export function CommitDetails({ commit }: { commit: CommitInfo }) {
  const id = useId();
  const subjectId = `${id}-subject`;
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [copyStatus, setCopyStatus] = useState<"idle" | "copied" | "failed">("idle");

  useLayoutEffect(() => {
    if (open) panel.current?.focus({ preventScroll: true });
  }, [open]);

  useEffect(() => {
    if (!open || copyStatus !== "copied") return;
    const timeout = window.setTimeout(() => setCopyStatus("idle"), 1200);
    return () => window.clearTimeout(timeout);
  }, [copyStatus, open]);

  useEffect(() => {
    if (!open) return;

    function closeOutside(event: PointerEvent) {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key !== "Escape" || event.isComposing) return;
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      trigger.current?.focus({ preventScroll: true });
    }

    document.addEventListener("pointerdown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  async function copyHash() {
    try {
      await navigator.clipboard.writeText(commit.hash);
      setCopyStatus("copied");
    } catch {
      setCopyStatus("failed");
    }
  }

  return (
    <div
      className="sc-commit-meta"
      ref={root}
      onBlur={(event) => {
        if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
    >
      <code title={commit.hash}>{commit.shortHash}</code>
      <time dateTime={commit.date} title={fullDate(commit.date)}>{relativeDate(commit.date)}</time>
      <button
        type="button"
        ref={trigger}
        className="ghost icon-button sc-commit-details-trigger"
        aria-label="Commit details"
        title="Commit details"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => {
          if (!open) setCopyStatus("idle");
          setOpen(!open);
        }}
      >
        <InformationRegular size={15} aria-hidden="true" />
      </button>
      {open && (
        <div
          id={id}
          className="sc-commit-details-panel"
          role="dialog"
          aria-labelledby={subjectId}
          tabIndex={-1}
          ref={panel}
        >
          <div className="sc-commit-message" role="region" aria-label="Commit message" tabIndex={0}>
            <h2 id={subjectId}>{commit.subject || "Untitled commit"}</h2>
            {commit.body && <p>{commit.body}</p>}
          </div>
          <footer className="sc-commit-details-footer">
            <div className="sc-commit-byline">
              <span>{commit.author} &lt;{commit.email}&gt;</span>
              <time dateTime={commit.date}>{fullDate(commit.date)}</time>
            </div>
            <div className="sc-commit-hash-row">
              <code>{commit.hash}</code>
              <button
                type="button"
                className="ghost icon-button"
                aria-label="Copy commit hash"
                title={copyStatus === "copied" ? "Commit hash copied" : "Copy commit hash"}
                onClick={() => void copyHash()}
              >
                {copyStatus === "copied"
                  ? <CheckRegular size={14} aria-hidden="true" />
                  : <CopyRegular size={14} aria-hidden="true" />}
              </button>
            </div>
            <span className={copyStatus === "failed" ? "sc-commit-copy-error" : "sr-only"} role="status">
              {copyStatus === "copied" ? "Commit hash copied." : copyStatus === "failed" ? "Could not copy the commit hash." : ""}
            </span>
          </footer>
        </div>
      )}
    </div>
  );
}
