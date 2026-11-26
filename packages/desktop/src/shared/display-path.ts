const VERBATIM_UNC_PREFIX = "\\\\?\\UNC\\";
const VERBATIM_PREFIX = "\\\\?\\";

export function displayPath(path: string): string {
  if (
    path.slice(0, VERBATIM_UNC_PREFIX.length).toUpperCase() ===
    VERBATIM_UNC_PREFIX
  ) {
    return `\\\\${path.slice(VERBATIM_UNC_PREFIX.length)}`;
  }
  return path.startsWith(VERBATIM_PREFIX)
    ? path.slice(VERBATIM_PREFIX.length)
    : path;
}
