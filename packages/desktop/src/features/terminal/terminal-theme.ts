/**
 * xterm colors read from the app theme's CSS custom properties.
 *
 * Keep the background opaque. xterm swaps the default foreground and
 * background for reverse-video cells (zsh's bracketed-paste highlight,
 * `standout`, `\x1b[7m`). With a transparent background the swapped text is
 * drawn transparent too, so pasted text disappears into the highlighted cell.
 * The terminal host paints the same `--bg` behind xterm, so opaque looks
 * identical.
 */
export function terminalTheme(read: (property: string) => string) {
  const color = (property: string) => read(property).trim();
  return {
    background: color("--bg"),
    foreground: color("--text"),
    cursor: color("--accent"),
    cursorAccent: color("--bg"),
    selectionBackground: color("--terminal-selection"),
    overviewRulerBorder: "rgba(0, 0, 0, 0)",
    black: color("--terminal-black"),
    red: color("--terminal-red"),
    green: color("--terminal-green"),
    yellow: color("--terminal-yellow"),
    blue: color("--terminal-blue"),
    magenta: color("--terminal-magenta"),
    cyan: color("--terminal-cyan"),
    white: color("--terminal-white"),
    brightBlack: color("--terminal-bright-black"),
    brightRed: color("--terminal-bright-red"),
    brightGreen: color("--terminal-bright-green"),
    brightYellow: color("--terminal-bright-yellow"),
    brightBlue: color("--terminal-bright-blue"),
    brightMagenta: color("--terminal-bright-magenta"),
    brightCyan: color("--terminal-bright-cyan"),
    brightWhite: color("--terminal-bright-white"),
  };
}
