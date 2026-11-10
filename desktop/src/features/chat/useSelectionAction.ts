import { useEffect, useRef, useState, type RefObject } from "react";

export type SelectionAction = {
  text: string;
  x: number;
  y: number;
  above: boolean;
};

/**
 * Position a "send to terminal" action over the current chat selection. The
 * listeners live only while the chat view is active.
 */
export function useSelectionAction({
  active,
  containerRef,
}: {
  active: boolean;
  containerRef: RefObject<HTMLElement | null>;
}) {
  const [selectionAction, setSelectionAction] = useState<SelectionAction | null>(
    null,
  );
  const selectionButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!active) {
      setSelectionAction(null);
      return;
    }
    let frame = 0;

    function showSelection() {
      const root = containerRef.current;
      const selection = window.getSelection();
      if (
        !root ||
        !selection ||
        selection.isCollapsed ||
        selection.rangeCount === 0
      ) {
        setSelectionAction(null);
        return;
      }
      const text = selection.toString().trim();
      const range = selection.getRangeAt(0);
      if (!text || !root.contains(range.commonAncestorContainer)) {
        setSelectionAction(null);
        return;
      }
      const rect = range.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      setSelectionAction({
        text,
        x: Math.min(Math.max(x, 80), window.innerWidth - 80),
        y: rect.top > 48 ? rect.top : rect.bottom,
        above: rect.top > 48,
      });
    }

    function scheduleSelection() {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(showSelection);
    }

    function onRelease(event: Event) {
      const target = event.target;
      if (
        target instanceof Node &&
        selectionButtonRef.current?.contains(target)
      ) {
        return;
      }
      scheduleSelection();
    }

    function hideSelection(event?: Event) {
      const target = event?.target;
      if (
        target instanceof Node &&
        selectionButtonRef.current?.contains(target)
      ) {
        return;
      }
      cancelAnimationFrame(frame);
      setSelectionAction(null);
    }

    function onSelectionChange() {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) hideSelection();
    }

    document.addEventListener("mousedown", hideSelection);
    document.addEventListener("keydown", hideSelection);
    document.addEventListener("selectstart", hideSelection);
    document.addEventListener("mouseup", onRelease);
    document.addEventListener("keyup", onRelease);
    document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("mousedown", hideSelection);
      document.removeEventListener("keydown", hideSelection);
      document.removeEventListener("selectstart", hideSelection);
      document.removeEventListener("mouseup", onRelease);
      document.removeEventListener("keyup", onRelease);
      document.removeEventListener("selectionchange", onSelectionChange);
    };
  }, [active, containerRef]);

  return {
    selectionAction,
    selectionButtonRef,
    clearSelection: () => setSelectionAction(null),
  };
}
