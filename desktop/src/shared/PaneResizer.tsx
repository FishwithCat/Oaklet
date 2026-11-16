import { useRef, type KeyboardEvent, type PointerEvent } from "react";

const MIN_WIDTH = 150;
const MAX_RATIO = 0.35;

function clampWidth(width: number, container: HTMLElement | null) {
  const available = container?.clientWidth ?? 900;
  return Math.max(MIN_WIDTH, Math.min(width, available * MAX_RATIO));
}

/** Draggable divider between a fixed-width sidebar and the remaining pane. */
export function PaneResizer({ width, label, onChange }: {
  width: number;
  label: string;
  onChange: (width: number) => void;
}) {
  const start = useRef<{ x: number; width: number } | null>(null);

  function startResize(event: PointerEvent<HTMLDivElement>) {
    start.current = { x: event.clientX, width };
    event.currentTarget.setPointerCapture(event.pointerId);
    document.body.classList.add("pane-resizing");
  }

  function dragResize(event: PointerEvent<HTMLDivElement>) {
    const drag = start.current;
    if (!drag) return;
    onChange(clampWidth(drag.width + event.clientX - drag.x, event.currentTarget.parentElement));
  }

  function stopResize(event: PointerEvent<HTMLDivElement>) {
    if (!start.current) return;
    start.current = null;
    document.body.classList.remove("pane-resizing");
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  function keyResize(event: KeyboardEvent<HTMLDivElement>) {
    const container = event.currentTarget.parentElement;
    const step = event.shiftKey ? 50 : 10;
    if (event.key === "ArrowLeft") onChange(clampWidth(width - step, container));
    else if (event.key === "ArrowRight") onChange(clampWidth(width + step, container));
    else if (event.key === "Home") onChange(MIN_WIDTH);
    else if (event.key === "End") onChange(clampWidth(Number.POSITIVE_INFINITY, container));
    else return;
    event.preventDefault();
  }

  return (
    <div
      className="pane-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      tabIndex={0}
      onPointerDown={startResize}
      onPointerMove={dragResize}
      onPointerUp={stopResize}
      onPointerCancel={stopResize}
      onLostPointerCapture={() => {
        start.current = null;
        document.body.classList.remove("pane-resizing");
      }}
      onKeyDown={keyResize}
    />
  );
}
