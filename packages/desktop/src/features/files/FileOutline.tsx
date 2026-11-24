import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ListOrderedRegular } from "@mingcute/react/core-regular";
import { errorText } from "../../shared/error-text";
import { qualifiedSymbol, type Outline } from "../../shared/source-symbol";

export function FileOutline({ workspace, path, source, dirty, busy, onJump }: {
  workspace: string;
  path: string;
  source: string;
  dirty: boolean;
  busy: boolean;
  onJump: (line: number) => void;
}) {
  const details = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const [result, setResult] = useState<{
    workspace: string; path: string; source: string; outline?: Outline; error?: string;
  } | null>(null);
  const current = result?.workspace === workspace && result.path === path && result.source === source
    ? result : null;

  useEffect(() => {
    if (!open || dirty || current) return;
    let cancelled = false;
    invoke<Outline>("files_outline", { workspace, path, content: source })
      .then((outline) => { if (!cancelled) setResult({ workspace, path, source, outline }); })
      .catch((error) => { if (!cancelled) setResult({ workspace, path, source, error: errorText(error) }); });
    return () => { cancelled = true; };
  }, [open, dirty, current, workspace, path, source]);

  return (
    <details ref={details} className="files-outline" onToggle={(event) => setOpen(event.currentTarget.open)}
      onKeyDown={(event) => {
        if (event.key === "Escape" && details.current) {
          details.current.open = false;
          details.current.querySelector("summary")?.focus();
        }
      }}>
      <summary className="ghost icon-button" title="File outline" aria-label="File outline">
        <ListOrderedRegular size={14} />
      </summary>
      {open && <div className="files-outline-panel" aria-label="File symbols">
        {dirty ? <p>Save changes to jump to a symbol.</p> :
          current?.error ? <p role="status">{current.error}</p> :
          !current?.outline ? <p role="status">Loading outline…</p> :
          <>
            {current.outline.hasErrors && <p>Syntax errors may leave the outline incomplete.</p>}
            {current.outline.symbols.length === 0 && <p>No declarations found.</p>}
            <ul>
              {current.outline.symbols.map((symbol, index) => (
                <li key={index}>
                  <button type="button" disabled={busy} onClick={() => {
                    if (details.current) details.current.open = false;
                    onJump(symbol.startLine);
                  }}>
                    <span title={qualifiedSymbol(symbol)}>{qualifiedSymbol(symbol)}</span>
                    <small>{symbol.kind} · {symbol.startLine}</small>
                  </button>
                </li>
              ))}
            </ul>
            {current.outline.truncated && <p>Outline truncated.</p>}
          </>}
      </div>}
    </details>
  );
}
