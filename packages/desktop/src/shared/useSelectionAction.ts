import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { createCompositionGuard, isImeEvent } from "../app/shortcuts";

type SelectionRect = Pick<DOMRect, "left" | "right" | "top" | "bottom">;
type SelectedText = { text: string; rect: SelectionRect };

export type SelectionAction = {
  text: string;
  x: number;
  y: number;
  above: boolean;
};

export function readTextRange(container: HTMLElement) {
  const selection = container.ownerDocument.getSelection();
  if (!selection?.rangeCount || selection.isCollapsed) return;
  const range = selection.getRangeAt(0);
  if (container.contains(range.commonAncestorContainer)) return range;
}

function readTextSelection(container: HTMLElement): SelectedText | undefined {
  const range = readTextRange(container);
  const text = range?.toString().trim();
  if (range && text) return { text, rect: range.getBoundingClientRect() };
}

export function useSelectionAction({
  active,
  containerRef,
  readSelection = readTextSelection,
  preferBelow = false,
}: {
  active: boolean;
  containerRef: RefObject<HTMLElement | null>;
  readSelection?: (container: HTMLElement) => SelectedText | undefined;
  preferBelow?: boolean;
}) {
  const [selectionAction, setSelectionAction] = useState<SelectionAction | null>(
    null,
  );
  const selectionButtonRef = useRef<HTMLButtonElement>(null);
  const frame = useRef(0);
  const clearSelection = useCallback(() => {
    cancelAnimationFrame(frame.current);
    setSelectionAction(null);
  }, []);

  useEffect(() => {
    if (!active) {
      clearSelection();
      return;
    }
    let pressedInside = false;
    const composition = createCompositionGuard();

    function showSelection(release?: { x: number; y: number }) {
      const root = containerRef.current;
      const selection = root && readSelection(root);
      if (!selection) {
        setSelectionAction(null);
        return;
      }
      const { text, rect } = selection;
      const clearance = preferBelow ? 12 : 6;
      const top = release ? release.y - clearance : rect.top;
      const bottom = release ? release.y + clearance : rect.bottom;
      const above = top >= 42 && (!preferBelow || window.innerHeight - bottom < 42);
      setSelectionAction({
        text,
        x: release?.x ?? (rect.left + rect.right) / 2,
        y: above ? top : bottom,
        above,
      });
    }

    function inside(target: EventTarget | null) {
      return target instanceof Node && containerRef.current?.contains(target);
    }

    function onButton(target: EventTarget | null) {
      return target instanceof Node && selectionButtonRef.current?.contains(target);
    }

    function onRelease(event: MouseEvent | KeyboardEvent) {
      const fromSelection = inside(event.target) || (event.type === "mouseup" && pressedInside);
      if (event.type === "mouseup") pressedInside = false;
      if (!fromSelection || onButton(event.target)) return;
      if (event instanceof KeyboardEvent) {
        const selectAll = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "a";
        const extendSelection = event.shiftKey &&
          (event.key.startsWith("Arrow") || ["Home", "End", "PageUp", "PageDown"].includes(event.key));
        if (composition.active() || isImeEvent(event) || (!selectAll && !extendSelection)) return;
      }
      const release = event instanceof MouseEvent ? { x: event.clientX, y: event.clientY } : undefined;
      cancelAnimationFrame(frame.current);
      frame.current = requestAnimationFrame(() => showSelection(release));
    }

    function hideSelection(event: Event) {
      if (!onButton(event.target)) clearSelection();
    }

    function onMouseDown(event: MouseEvent) {
      pressedInside = !!inside(event.target);
      hideSelection(event);
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") clearSelection();
      else if (event.key !== "Tab") hideSelection(event);
    }

    function onSelectionChange() {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) clearSelection();
    }

    function onFocus(event: FocusEvent) {
      if (inside(event.target)) return;
      composition.reset();
      hideSelection(event);
    }

    function onCompositionStart(event: CompositionEvent) {
      if (!inside(event.target)) return;
      composition.start();
      clearSelection();
    }

    function onScroll(event: Event) {
      const root = containerRef.current;
      const target = event.target;
      if (root && target instanceof Node && (root.contains(target) || target.contains(root))) {
        clearSelection();
      }
    }

    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("selectstart", hideSelection);
    document.addEventListener("compositionstart", onCompositionStart);
    document.addEventListener("compositionend", composition.end);
    document.addEventListener("mouseup", onRelease);
    document.addEventListener("keyup", onRelease);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", clearSelection);
    if (readSelection === readTextSelection) document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      cancelAnimationFrame(frame.current);
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("selectstart", hideSelection);
      document.removeEventListener("compositionstart", onCompositionStart);
      document.removeEventListener("compositionend", composition.end);
      document.removeEventListener("mouseup", onRelease);
      document.removeEventListener("keyup", onRelease);
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", clearSelection);
      document.removeEventListener("selectionchange", onSelectionChange);
    };
  }, [active, clearSelection, containerRef, preferBelow, readSelection]);

  return {
    selectionAction: active ? selectionAction : null,
    selectionButtonRef,
    clearSelection,
  };
}
