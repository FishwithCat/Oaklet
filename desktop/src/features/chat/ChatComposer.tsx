import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
} from "react";
import {
  PaperPlaneRightIcon,
  PlantIcon,
  QuestionIcon,
} from "@phosphor-icons/react";
import { isSendEnter } from "../../app/shortcuts";
import { contextTier, formatContextUsage } from "./chat-model";
import type { ContextUsage } from "./session-model";

export type ChatComposerHandle = {
  focus: () => void;
};

function ContextGauge({
  usage,
  running,
  compacting,
  onCompact,
}: {
  usage: ContextUsage;
  running: boolean;
  compacting: boolean;
  onCompact: () => void;
}) {
  const label = formatContextUsage(usage.used, usage.window);
  const tier = contextTier(usage.used, usage.window);
  const hint = running
    ? "Stop the run to compact context"
    : compacting
      ? "Compacting context…"
      : "Click to compact context";
  return (
    <button
      type="button"
      className={`context-meter${tier === "normal" ? "" : ` ${tier}`}${
        compacting ? " compacting" : ""
      }`}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onCompact}
      disabled={running || compacting}
      aria-label={`Context used: ${label}. ${hint}`}
      title={`${label} — ${hint}`}
    >
      <PlantIcon size={18} weight="duotone" aria-hidden="true" />
    </button>
  );
}

export function ChatComposer({
  ref,
  activeTabId,
  value,
  running,
  compacting,
  context,
  onChange,
  onSubmit,
  onCompact,
}: {
  ref?: Ref<ChatComposerHandle>;
  activeTabId: string;
  value: string;
  running: boolean;
  compacting: boolean;
  context: ContextUsage | null;
  onChange: (value: string) => void;
  onSubmit: () => boolean;
  onCompact: () => void;
}) {
  const [helpOpen, setHelpOpen] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const helpPopoverRef = useRef<HTMLDivElement>(null);
  const composingRef = useRef(false);

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
      if (event.key === "Escape") setHelpOpen(false);
    }
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [helpOpen]);

  useLayoutEffect(() => {
    setHelpOpen(false);
  }, [activeTabId]);

  useLayoutEffect(() => {
    resizeComposer();
  }, [activeTabId, value]);

  useImperativeHandle(ref, () => ({
    focus: () => textareaRef.current?.focus(),
  }), []);

  function resizeComposer() {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 180)}px`;
  }

  function submit() {
    if (compacting) return;
    if (!onSubmit()) return;
    if (textareaRef.current) textareaRef.current.style.height = "auto";
  }

  return (
    <div className="composer-wrap">
      <form
        className={`composer${context ? " has-context" : ""}`}
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        {context && (
          <ContextGauge
            usage={context}
            running={running}
            compacting={compacting}
            onCompact={onCompact}
          />
        )}
        <textarea
          ref={textareaRef}
          rows={1}
          value={value}
          placeholder="Ask Oaklet to build, fix, or explain something..."
          onChange={(event) => {
            onChange(event.target.value);
            resizeComposer();
          }}
          onCompositionStart={() => {
            composingRef.current = true;
          }}
          onCompositionEnd={() => {
            // Safari fires compositionend before the Enter keydown that
            // confirms the candidate, so keep the flag through that keydown.
            setTimeout(() => {
              composingRef.current = false;
            }, 0);
          }}
          onBlur={() => {
            composingRef.current = false;
          }}
          onKeyDown={(event) => {
            if (!isSendEnter(event.nativeEvent, composingRef.current)) return;
            event.preventDefault();
            submit();
          }}
        />
        <div className="composer-footer">
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
              aria-controls="shortcuts-help"
            >
              <QuestionIcon size={11} />
              Shortcuts
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
                  <div className="shortcut">
                    <dt>
                      <kbd>⌘ N</kbd>
                    </dt>
                    <dd>New session</dd>
                  </div>
                  <div className="shortcut">
                    <dt>
                      <kbd>⌘ R</kbd>
                    </dt>
                    <dd>Search history</dd>
                  </div>
                  <div className="shortcut">
                    <dt>
                      <kbd>⌃ C</kbd>
                    </dt>
                    <dd>Stop run</dd>
                  </div>
                  <div className="shortcut">
                    <dt>
                      <kbd>↵</kbd>
                    </dt>
                    <dd>Send</dd>
                  </div>
                  <div className="shortcut">
                    <dt>
                      <kbd>⇧ ↵</kbd>
                    </dt>
                    <dd>New line</dd>
                  </div>
                </dl>
              </div>
            )}
          </span>
          <button
            type="submit"
            className="primary icon-button"
            disabled={running || compacting || !value.trim()}
            aria-label={compacting ? "Compacting context" : "Send"}
            title={compacting ? "Compacting context…" : "Send"}
          >
            <PaperPlaneRightIcon size={16} weight="fill" />
          </button>
        </div>
      </form>
    </div>
  );
}
