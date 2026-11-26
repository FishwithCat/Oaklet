export async function fileLanguage(path: string) {
  const ext = /[^/\\]\.([a-z]+)$/i.exec(path)?.[1].toLowerCase();
  switch (ext) {
    case "js": case "jsx": case "mjs": case "cjs":
      return (await import("@codemirror/lang-javascript")).javascript({ jsx: ext === "jsx" });
    case "ts": case "tsx": case "mts": case "cts":
      return (await import("@codemirror/lang-javascript")).javascript({ typescript: true, jsx: ext === "tsx" });
    case "json": return (await import("@codemirror/lang-json")).json();
    case "md": case "mdx": return (await import("@codemirror/lang-markdown")).markdown();
    case "rs": return (await import("@codemirror/lang-rust")).rust();
    case "py": return (await import("@codemirror/lang-python")).python();
    case "java": return (await import("@codemirror/lang-java")).java();
    case "css": return (await import("@codemirror/lang-css")).css();
    case "html": case "htm": return (await import("@codemirror/lang-html")).html();
  }
}
