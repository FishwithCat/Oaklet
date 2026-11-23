import {
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
} from "react";

const DRAG_THRESHOLD = 4;

/** Move one item, returning the same array when the index does not change. */
export function moveItem<T>(items: T[], from: number, to: number): T[] {
  if (
    from === to ||
    from < 0 ||
    to < 0 ||
    from >= items.length ||
    to >= items.length
  ) {
    return items;
  }
  const next = [...items];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

/**
 * Pointer-driven reordering for the tab strips. `onMove` receives the dragged
 * tab's index and its new index; the tab under the pointer is the drop target.
 * Moves are observed on the window, so re-inserting the dragged tab cannot
 * cancel the gesture.
 */
export function useTabDrag(onMove: (from: number, to: number) => void) {
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const drag = useRef<{ id: string; startX: number; tab: HTMLElement } | null>(
    null,
  );
  const moved = useRef(false);
  const suppressClick = useRef(false);

  function move(event: globalThis.PointerEvent) {
    const current = drag.current;
    if (!current) return;
    if (!moved.current) {
      if (Math.abs(event.clientX - current.startX) < DRAG_THRESHOLD) return;
      moved.current = true;
      suppressClick.current = true;
      setDraggingId(current.id);
    }
    const strip = current.tab.parentElement;
    if (!strip) return;
    const children = [...strip.children];
    const from = children.indexOf(current.tab);
    const rest = children.filter((child) => child !== current.tab);
    const target = rest.findIndex((child) => {
      const rect = child.getBoundingClientRect();
      return event.clientX < rect.left + rect.width / 2;
    });
    onMove(from, target < 0 ? rest.length : target);
  }

  function stop() {
    if (!drag.current) return;
    drag.current = null;
    moved.current = false;
    setDraggingId(null);
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", stop);
    window.removeEventListener("pointercancel", stop);
    window.removeEventListener("blur", stop);
  }

  function start(id: string, event: PointerEvent<HTMLElement>) {
    suppressClick.current = false;
    if (
      event.button !== 0 ||
      (event.target as HTMLElement).closest(".view-tab-close")
    ) {
      return;
    }
    drag.current = { id, startX: event.clientX, tab: event.currentTarget };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    window.addEventListener("blur", stop);
  }

  function click(event: MouseEvent<HTMLElement>) {
    if (!suppressClick.current) return;
    suppressClick.current = false;
    event.preventDefault();
    event.stopPropagation();
  }

  function keyDown(event: KeyboardEvent<HTMLElement>) {
    if (!event.altKey) return;
    const tab = event.currentTarget;
    const strip = tab.parentElement;
    if (!strip) return;
    const from = [...strip.children].indexOf(tab);
    const to =
      from +
      (event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0);
    if (to === from || to < 0 || to >= strip.children.length) return;
    event.preventDefault();
    onMove(from, to);
  }

  return {
    draggingId,
    dragProps: (id: string) => ({
      onPointerDown: (event: PointerEvent<HTMLElement>) => start(id, event),
      onClickCapture: click,
      onKeyDown: keyDown,
    }),
  };
}
