/** Turn selected chat text into input for the PTY, pressing Enter unless it
 * already ends with one. */
export function commandForTerminal(text: string): string | null {
  const command = text.trim();
  if (!command) return null;
  return `${command}\r`;
}
