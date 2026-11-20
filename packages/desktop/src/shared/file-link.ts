const LINK_SCHEME = /^[a-z][a-z0-9+.-]*:/i;
// A bare name is usually a doc or config; source files normally come with a
// path. Keeping `Node.js` out matters more than linking a bare `App.tsx`.
const BARE_FILE =
  /\.(?:md|mdx|markdown|txt|json|jsonc|ya?ml|toml|lock|csv|pdf|html?|css|scss|sass|less|xml|svg|png|jpe?g|gif|webp|ini|conf|cfg|env|sql|sh|bash|zsh|ps1)$/i;
const PATH_FILE =
  /\.(?:md|mdx|markdown|txt|json|jsonc|ya?ml|toml|lock|csv|pdf|html?|css|scss|sass|less|xml|svg|png|jpe?g|gif|webp|ini|conf|cfg|env|sql|sh|bash|zsh|ps1|ts|tsx|js|jsx|mjs|cjs|rs|py|go|java|kt|swift|cs|rb|php)$/i;

/**
 * Turn a markdown link into a workspace-relative file path, or null when the
 * link is not a plain relative path (external URL, anchor, absolute path, or
 * one that climbs out of the workspace). The Rust file command repeats the
 * workspace check; this one keeps dead ends out of the UI.
 */
export function workspaceFileLink(href: string): string | null {
  const value = href.trim();
  if (
    !value ||
    value.startsWith("#") ||
    value.startsWith("/") ||
    value.startsWith("\\") ||
    LINK_SCHEME.test(value)
  ) {
    return null;
  }

  const withoutFragment = value.split(/[?#]/, 1)[0] ?? "";
  let decoded: string;
  try {
    decoded = decodeURIComponent(withoutFragment);
  } catch {
    return null;
  }

  if (decoded.endsWith("/") || decoded.endsWith("\\")) return null;
  const parts = decoded.split(/[/\\]/).filter((part) => part && part !== ".");
  if (parts.length === 0 || parts.includes("..")) return null;
  return parts.join("/");
}

/**
 * A path written as inline code, such as `TECH_STACK.md` or `src/main.ts`.
 * Agents name files in code spans far more often than they write markdown
 * links. The extension allowlist keeps `React.Component` and `v1.2.3` plain.
 */
export function inlineCodeFile(text: string): string | null {
  if (/\s/.test(text)) return null;
  const path = workspaceFileLink(text);
  if (!path) return null;
  const file = path.includes("/") ? PATH_FILE : BARE_FILE;
  return file.test(path) ? path : null;
}
