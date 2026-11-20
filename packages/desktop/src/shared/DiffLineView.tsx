import { useState } from "react";

/** One drawable diff row. Line numbers stay optional: git patches have them,
 *  tool-call fragments do not. */
export type DiffLineRow = {
  kind: "add" | "del" | "context" | "meta";
  text: string;
  oldLine?: number | null;
  newLine?: number | null;
};

const COPY_RESET_MS = 1200;

function reference(path: string | undefined, line: number): string {
  return path ? `${path}:${line}` : String(line);
}

async function copyText(text: string): Promise<boolean> {
  const write = navigator.clipboard?.writeText(text);
  if (!write) return false;
  return write.then(
    () => true,
    () => false,
  );
}

/**
 * One diff row: optional old/new gutters, sign, text. `numbers` is opt-in
 * because only git patches carry file line numbers.
 */
export function DiffLineView({
  line,
  path,
  numbers = false,
  wrap = false,
}: {
  line: DiffLineRow;
  path?: string;
  numbers?: boolean;
  wrap?: boolean;
}) {
  const [copied, setCopied] = useState<"old" | "new" | null>(null);
  const oldLine = line.oldLine ?? null;
  const newLine = line.newLine ?? null;

  const copy = (side: "old" | "new", value: number) => {
    void copyText(reference(path, value)).then((ok) => {
      if (!ok) return;
      setCopied(side);
      window.setTimeout(() => {
        setCopied((current) => (current === side ? null : current));
      }, COPY_RESET_MS);
    });
  };

  const gutter = (side: "old" | "new", value: number | null) => {
    if (!numbers) return null;
    if (value === null) {
      return <span className={`diff-num ${side} blank`} aria-hidden="true" />;
    }
    return (
      <span
        className={`diff-num ${side}${copied === side ? " copied" : ""}`}
        aria-hidden="true"
        title={`Copy ${reference(path, value)}`}
        onClick={() => copy(side, value)}
      >
        {copied === side ? "✓" : value}
      </span>
    );
  };

  const sides = [
    oldLine === null ? null : `old ${oldLine}`,
    newLine === null ? null : `new ${newLine}`,
  ].filter(Boolean);
  const title = numbers && sides.length > 0 ? sides.join(" · ") : undefined;

  return (
    <div
      className={`diff-line ${line.kind}${numbers ? " numbered" : ""}${
        wrap ? " wrap" : ""
      }`}
      title={title}
    >
      {gutter("old", oldLine)}
      {gutter("new", newLine)}
      <span className="diff-sign" aria-hidden="true">
        {line.kind === "add" ? "+" : line.kind === "del" ? "-" : " "}
      </span>
      <span className="diff-text">{line.text === "" ? " " : line.text}</span>
    </div>
  );
}
