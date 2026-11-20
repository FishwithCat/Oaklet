/** Minimum visible thumb height, so a long scrollback stays draggable. */
const MIN_THUMB = 24;

export type ScrollbarGeometry = {
  hidden: boolean;
  top: number;
  height: number;
};

/**
 * Map the xterm viewport (rows visible of `total` buffer lines) onto a
 * full-height track. The track spans the whole pane, so the thumb reaches the
 * bottom edge instead of stopping above the terminal's bottom padding.
 */
export function scrollbarGeometry(
  viewportY: number,
  rows: number,
  total: number,
  track: number,
): ScrollbarGeometry {
  if (track <= 0 || total <= rows) return { hidden: true, top: 0, height: 0 };
  const height = Math.min(
    track,
    Math.max(MIN_THUMB, Math.round((rows / total) * track)),
  );
  const maxTop = track - height;
  const top = Math.min(
    Math.max(Math.round((viewportY / (total - rows)) * maxTop), 0),
    maxTop,
  );
  return { hidden: false, top, height };
}
