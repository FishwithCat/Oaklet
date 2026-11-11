import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type Ref,
} from "react";
import {
  CloseRegular,
  FileRegular,
  FlowerpotRegular,
  QuestionRegular,
} from "@mingcute/react/core-regular";
import { SendFilled } from "@mingcute/react/core-filled";
import { isSendEnter } from "../../app/shortcuts";
import { errorText } from "../../shared/error-text";
import {
  formatBytes,
  readAttachment,
  type Attachment,
} from "./attachments";
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
  usage: ContextUsage | null;
  running: boolean;
  compacting: boolean;
  onCompact: () => void;
}) {
  const label = usage ? formatContextUsage(usage.used, usage.window) : null;
  const tier = usage ? contextTier(usage.used, usage.window) : "normal";
  const hint = running
    ? "Stop the run to compact context"
    : compacting
      ? "Compacting context…"
      : usage
        ? "Click to compact context"
        : "Context usage is unavailable; click to compact context";
  return (
    <button
      type="button"
      className={`context-meter${tier === "normal" ? "" : ` ${tier}`}${
        compacting ? " compacting" : ""
      }`}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onCompact}
      disabled={running || compacting}
      aria-label={
        label
          ? `Context used: ${label}. ${hint}`
          : `Context usage unavailable. ${hint}`
      }
      title={
        label
          ? `${label} — ${hint}`
          : `Context usage unavailable — ${hint}`
      }
    >
      <FlowerpotRegular size={18} aria-hidden="true" />
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
  showContextGauge,
  attachments,
  onChange,
  onAddAttachments,
  onRemoveAttachment,
  onSubmit,
  onCompact,
}: {
  ref?: Ref<ChatComposerHandle>;
  activeTabId: string;
  value: string;
  running: boolean;
  compacting: boolean;
  context: ContextUsage | null;
  showContextGauge: boolean;
  attachments: Attachment[];
  onChange: (value: string) => void;
  onAddAttachments: (attachments: Attachment[]) => void;
  onRemoveAttachment: (index: number) => void;
  onSubmit: (attachments: Attachment[]) => boolean;
  onCompact: () => void;
}) {
  const [helpOpen, setHelpOpen] = useState(false);
  const [attachmentError, setAttachmentError] = useState("");
  const [reading, setReading] = useState(false);
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
    setAttachmentError("");
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

  async function pasteFiles(event: ClipboardEvent<HTMLTextAreaElement>) {
    // Safari exposes pasted files through `items`; Chromium fills `files`.
    const fromItems = Array.from(event.clipboardData.items)
      .filter((item) => item.kind === "file")
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    const files =
      fromItems.length > 0 ? fromItems : Array.from(event.clipboardData.files);
    // Plain text falls through to the browser's default paste.
    if (files.length === 0) return;
    event.preventDefault();
    setAttachmentError("");
    setReading(true);
    try {
      const added: Attachment[] = [];
      for (const file of files) {
        try {
          added.push(await readAttachment(file));
        } catch (error) {
          setAttachmentError(errorText(error));
        }
      }
      if (added.length > 0) {
        onAddAttachments(added);
      }
    } finally {
      setReading(false);
    }
  }

  function submit() {
    if (compacting || reading) return;
    if (!onSubmit(attachments)) return;
    setAttachmentError("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
  }

  return (
    <div className="composer-wrap">
      <form
        className={`composer${showContextGauge ? " has-context" : ""}`}
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        {showContextGauge && (
          <ContextGauge
            usage={context}
            running={running}
            compacting={compacting}
            onCompact={onCompact}
          />
        )}
        {attachments.length > 0 && (
          <ul className="composer-attachments" aria-label="Attachments">
            {attachments.map((attachment, index) => (
              <li
                className={`composer-attachment ${attachment.kind}`}
                key={`${attachment.name}-${index}`}
              >
                {attachment.kind === "image" ? (
                  <img src={attachment.thumbnail} alt="" />
                ) : (
                  <span className="composer-attachment-icon" aria-hidden="true">
                    <FileRegular size={14} />
                  </span>
                )}
                <span className="composer-attachment-name" title={attachment.name}>
                  {attachment.name}
                </span>
                <span className="composer-attachment-size">
                  {formatBytes(attachment.bytes)}
                </span>
                <button
                  type="button"
                  className="composer-attachment-remove"
                  onClick={() => onRemoveAttachment(index)}
                  aria-label={`Remove ${attachment.name}`}
                  title="Remove attachment"
                >
                  <CloseRegular size={10} />
                </button>
              </li>
            ))}
          </ul>
        )}
        {attachmentError && (
          <div className="composer-error" role="alert">
            {attachmentError}
          </div>
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
          onPaste={pasteFiles}
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
              <QuestionRegular size={11} />
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
                  <div className="shortcut">
                    <dt>
                      <kbd>⌘ V</kbd>
                    </dt>
                    <dd>Paste image or file</dd>
                  </div>
                </dl>
              </div>
            )}
          </span>
          <button
            type="submit"
            className="primary icon-button"
            disabled={
              running ||
              compacting ||
              reading ||
              (!value.trim() && attachments.length === 0)
            }
            aria-label={compacting ? "Compacting context" : "Send"}
            title={compacting ? "Compacting context…" : "Send"}
          >
            <SendFilled size={16} />
          </button>
        </div>
      </form>
    </div>
  );
}
