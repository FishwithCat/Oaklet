import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { KeyboardRegular } from "@mingcute/react/core-regular";
import { primaryKeyLabel } from "../../app/shortcuts";

export function ComposerShortcuts({ activeTabId, running, welcome }: {
  activeTabId: string;
  running: boolean;
  welcome?: boolean;
}) {
  const [helpOpen, setHelpOpen] = useState(false);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const helpPopoverRef = useRef<HTMLDivElement>(null);
  const modifierKey = primaryKeyLabel();

  useEffect(() => {
    if (!helpOpen) return;
    function onMouseDown(event: MouseEvent) {
      const target = event.target as Node;
      if (
        !helpButtonRef.current?.contains(target) &&
        !helpPopoverRef.current?.contains(target)
      ) {
        setHelpOpen(false);
      }
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setHelpOpen(false);
      }
    }
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [helpOpen]);

  useLayoutEffect(() => {
    setHelpOpen(false);
  }, [activeTabId]);

  return (
    <span className="shortcut-trigger-wrap">
      <button
        ref={helpButtonRef}
        type="button"
        className="shortcut-trigger"
        onMouseEnter={() => setHelpOpen(true)}
        onMouseLeave={() => setHelpOpen(false)}
        onFocus={() => setHelpOpen(true)}
        onBlur={() => setHelpOpen(false)}
        onClick={() => setHelpOpen(true)}
        aria-label="Keyboard shortcuts"
        aria-expanded={helpOpen}
        aria-controls={helpOpen ? "shortcuts-help" : undefined}
      >
        <KeyboardRegular
          size={welcome ? 11 : 15}
          aria-hidden="true"
        />
        {welcome && "Shortcuts"}
      </button>
      {helpOpen && (
        <div
          className="shortcuts-popover"
          id="shortcuts-help"
          ref={helpPopoverRef}
          role="dialog"
          aria-label="Keyboard shortcuts"
        >
          <h3>Shortcuts</h3>
          <dl>
            {!welcome && (
              <div className="shortcut">
                <dt>
                  <kbd>{modifierKey} N</kbd>
                </dt>
                <dd>New session</dd>
              </div>
            )}
            {!welcome && (
              <div className="shortcut">
                <dt>
                  <kbd>{modifierKey} {modifierKey === "⌘" ? "X" : "W"}</kbd>
                </dt>
                <dd>Close tab</dd>
              </div>
            )}
            <div className="shortcut">
              <dt>
                <kbd>{modifierKey} R</kbd>
              </dt>
              <dd>Search history</dd>
            </div>
            {!welcome && (
              <div className="shortcut">
                <dt>
                  <kbd>{modifierKey} F</kbd>
                </dt>
                <dd>Find in chat</dd>
              </div>
            )}
            {running && (
              <div className="shortcut">
                <dt>
                  <kbd>Ctrl C</kbd>
                </dt>
                <dd>Stop run</dd>
              </div>
            )}
            <div className="shortcut">
              <dt>
                <kbd>↵</kbd>
              </dt>
              <dd>{running ? "Steer the current turn" : "Send"}</dd>
            </div>
            <div className="shortcut">
              <dt>
                <kbd>⇧ ↵</kbd>
              </dt>
              <dd>New line</dd>
            </div>
            <div className="shortcut">
              <dt>
                <kbd>{modifierKey} V</kbd>
              </dt>
              <dd>Paste image or file</dd>
            </div>
          </dl>
        </div>
      )}
    </span>
  );
}
