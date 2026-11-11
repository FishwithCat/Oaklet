import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ArrowDownRegular,
  ArrowUpRegular,
  CloseRegular,
  SearchRegular,
} from "@mingcute/react/core-regular";
import {
  findMatches,
  locateOffset,
  stepMatch,
  type TextRuns,
} from "./find-model";

const ALL_HIGHLIGHT = "chat-find";
const CURRENT_HIGHLIGHT = "chat-find-current";

type Results = { query: string; ranges: Range[] };

/**
 * Matches are collected per block so a query can span inline formatting
 * (bold, code, links) without leaking across paragraphs.
 */
const BLOCK_SELECTOR =
  "p, li, h1, h2, h3, h4, h5, h6, pre, blockquote, td, th, .bubble-text, .notice, .row-title, .row-summary";

type TextUnit = TextRuns & { text: string; nodes: Text[] };

function locate(unit: TextUnit, offset: number) {
  const found = locateOffset(unit, offset);
  if (!found) return null;
  const node = unit.nodes[found.index];
  if (!node) return null;
  return { node, offset: found.offset };
}

function collectRanges(root: HTMLElement | null, query: string): Range[] {
  if (!root || !query) return [];
  const units = new Map<Element, TextUnit>();
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const element = node.parentElement;
    // Screen-reader-only status labels are not part of the visible thread.
    if (!element || element.closest(".sr-only")) continue;
    const key = element.closest(BLOCK_SELECTOR) ?? element;
    const unit = units.get(key) ?? {
      text: "",
      nodes: [],
      starts: [],
      lengths: [],
    };
    unit.starts.push(unit.text.length);
    unit.lengths.push(node.textContent?.length ?? 0);
    unit.nodes.push(node as Text);
    unit.text += node.textContent ?? "";
    units.set(key, unit);
  }
  const ranges: Range[] = [];
  for (const unit of units.values()) {
    for (const match of findMatches(unit.text, query)) {
      const start = locate(unit, match.start);
      const end = locate(unit, match.end);
      if (!start || !end) continue;
      const range = document.createRange();
      range.setStart(start.node, start.offset);
      range.setEnd(end.node, end.offset);
      ranges.push(range);
    }
  }
  return ranges;
}

function paintHighlights(results: Results, index: number) {
  const registry = CSS.highlights;
  if (!registry) return;
  registry.delete(ALL_HIGHLIGHT);
  registry.delete(CURRENT_HIGHLIGHT);
  if (results.ranges.length === 0) return;
  registry.set(ALL_HIGHLIGHT, new Highlight(...results.ranges));
  const active = results.ranges[index];
  if (!active) return;
  const current = new Highlight(active);
  current.priority = 1;
  registry.set(CURRENT_HIGHLIGHT, current);
}

function clearHighlights() {
  CSS.highlights?.delete(ALL_HIGHLIGHT);
  CSS.highlights?.delete(CURRENT_HIGHLIGHT);
}

/** Command+F find bar over the rendered chat thread. */
export function ChatFind({
  active,
  tabId,
  containerRef,
  items,
}: {
  active: boolean;
  tabId: string;
  containerRef: RefObject<HTMLElement | null>;
  items: unknown[];
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const [results, setResults] = useState<Results>({ query: "", ranges: [] });
  const inputRef = useRef<HTMLInputElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const scrolledRef = useRef("");

  useEffect(() => {
    if (!active) {
      setOpen(false);
      return;
    }
    function onKeyDown(event: KeyboardEvent) {
      if (document.querySelector(".modal-backdrop, .skills-backdrop")) return;
      if (
        event.metaKey &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.shiftKey &&
        event.key.toLowerCase() === "f"
      ) {
        event.preventDefault();
        if (!open) {
          previousFocusRef.current =
            document.activeElement instanceof HTMLElement
              ? document.activeElement
              : null;
        }
        setIndex(0);
        setOpen(true);
        if (open) {
          inputRef.current?.focus();
          inputRef.current?.select();
        }
      } else if (event.key === "Escape" && open) {
        event.preventDefault();
        setOpen(false);
      }
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [active, open]);

  useEffect(() => {
    if (open) {
      inputRef.current?.focus();
      inputRef.current?.select();
    } else if (active) {
      previousFocusRef.current?.focus();
      previousFocusRef.current = null;
    }
  }, [open, active]);

  useEffect(() => {
    // Re-read the DOM when the thread streams in or the tab changes.
    const ranges = open ? collectRanges(containerRef.current, query) : [];
    setResults({ query, ranges });
    setIndex((current) =>
      ranges.length === 0 ? 0 : Math.min(current, ranges.length - 1),
    );
  }, [open, query, items, tabId, containerRef]);

  useEffect(() => {
    paintHighlights(results, index);
    return clearHighlights;
  }, [results, index]);

  useEffect(() => {
    if (!open || results.query !== query) {
      scrolledRef.current = "";
      return;
    }
    const range = results.ranges[index];
    if (!range) return;
    // Keep the viewport still when matches are re-collected by streaming.
    const key = `${tabId}:${index}:${query}`;
    if (scrolledRef.current === key) return;
    scrolledRef.current = key;
    range.startContainer.parentElement?.scrollIntoView({ block: "center" });
  }, [open, query, results, index, tabId]);

  function step(delta: number) {
    setIndex((current) => stepMatch(current, results.ranges.length, delta));
  }

  if (!open) return null;
  const count = results.ranges.length;

  return (
    <div className="chat-find" role="search" aria-label="Find in chat">
      <SearchRegular size={13} className="chat-find-icon" aria-hidden="true" />
      <input
        ref={inputRef}
        className="chat-find-input"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          setIndex(0);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            step(event.shiftKey ? -1 : 1);
          } else if (event.key === "Escape") {
            event.preventDefault();
            setOpen(false);
          }
        }}
        placeholder="Find in chat"
        aria-label="Find in chat"
        autoComplete="off"
        spellCheck={false}
      />
      <span className="chat-find-count" aria-live="polite">
        {query ? `${count ? index + 1 : 0}/${count}` : ""}
      </span>
      <button
        type="button"
        className="chat-find-step"
        onClick={() => step(-1)}
        disabled={count === 0}
        aria-label="Previous match"
        title="Previous match (⇧ Enter)"
      >
        <ArrowUpRegular size={14} />
      </button>
      <button
        type="button"
        className="chat-find-step"
        onClick={() => step(1)}
        disabled={count === 0}
        aria-label="Next match"
        title="Next match (Enter)"
      >
        <ArrowDownRegular size={14} />
      </button>
      <button
        type="button"
        className="chat-find-step chat-find-close"
        onClick={() => setOpen(false)}
        aria-label="Close find"
        title="Close (Esc)"
      >
        <CloseRegular size={16} />
      </button>
    </div>
  );
}
