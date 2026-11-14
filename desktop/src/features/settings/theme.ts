import { useEffect, useState } from "react";

export type Theme = "light" | "dark" | "auto";
export type ResolvedTheme = Exclude<Theme, "auto">;

/** Auto follows the OS setting; explicit modes ignore it. */
export function resolveTheme(theme: Theme, prefersDark: boolean): ResolvedTheme {
  if (theme === "auto") return prefersDark ? "dark" : "light";
  return theme;
}

/** Paint the resolved theme. CSS and code-owned surfaces read `data-theme`. */
export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = resolveTheme(
    theme,
    window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
}

/** Follow live theme changes in surfaces that keep their own colors. */
export function useResolvedTheme(): ResolvedTheme {
  const [theme, setTheme] = useState<ResolvedTheme>(() =>
    document.documentElement.dataset.theme === "dark" ? "dark" : "light",
  );
  useEffect(() => {
    const observer = new MutationObserver(() => {
      setTheme(
        document.documentElement.dataset.theme === "dark" ? "dark" : "light",
      );
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => observer.disconnect();
  }, []);
  return theme;
}
