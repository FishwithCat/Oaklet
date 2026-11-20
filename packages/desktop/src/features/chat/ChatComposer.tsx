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
import { PauseFilled, SendFilled } from "@mingcute/react/core-filled";
import { isSendEnter } from "../../app/shortcuts";
import { errorText } from "../../shared/error-text";
import {
  formatBytes,
  readAttachment,
  type Attachment,
} from "./attachments";
import {
  contextTier,
  countUpValue,
  formatContextLabel,
  formatContextUsage,
  formatModelId,
  formatRequestTitle,
  formatUsageTitle,
} from "./chat-model";
import type { ContextUsage, RunUsage, StepUsage } from "./session-model";

export type ChatComposerHandle = {
  focus: () => void;
};

/** Count-up digits: the context size eases to its new value instead of jumping. */
function UsageLabel({
  used,
  limit,
  step,
}: {
  used: number;
  limit: number;
  step: StepUsage | null;
}) {
  const [shown, setShown] = useState(used);
  const shownRef = useRef(used);

  useEffect(() => {
    const from = shownRef.current;
    if (from === used) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      shownRef.current = used;
      setShown(used);
      return;
    }
    const started = performance.now();
    let frame = 0;
    const tick = (now: number) => {
      const value = countUpValue(from, used, (now - started) / 450);
      shownRef.current = value;
      setShown(value);
      if (value !== used) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [used]);

  return <>{formatContextLabel(shown, limit, step)}</>;
}

function ContextGauge({
  usage,
  runUsage,
  running,
  compacting,
  onCompact,
}: {
  usage: ContextUsage | null;
  runUsage: RunUsage | null;
  running: boolean;
  compacting: boolean;
  onCompact: () => void;
}) {
  const label = usage ? formatContextUsage(usage.used, usage.window) : null;
  const tokens =
    runUsage && runUsage.promptTokens > 0
      ? formatUsageTitle(runUsage)
      : null;
  const tier = usage ? contextTier(usage.used, usage.window) : "normal";
  const hint = running
    ? "Stop the run to compact context"
    : compacting
      ? "Click to stop compacting"
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
      disabled={running}
      aria-label={
        label
          ? `Context used: ${label}.${tokens ? ` ${tokens}.` : ""} ${hint}`
          : `Context usage unavailable.${tokens ? ` ${tokens}.` : ""} ${hint}`
      }
      title={
        label || tokens
          ? `${[label, tokens].filter(Boolean).join(" · ")} — ${hint}`
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
  usage,
  stepUsage,
  showContextGauge,
  model,
  welcome,
  attachments,
  onChange,
  onAddAttachments,
  onRemoveAttachment,
  onSubmit,
  onStop,
  onCompact,
}: {
  ref?: Ref<ChatComposerHandle>;
  activeTabId: string;
  value: string;
  running: boolean;
  compacting: boolean;
  context: ContextUsage | null;
  usage: RunUsage | null;
  stepUsage: StepUsage | null;
  showContextGauge: boolean;
  model: string;
  welcome?: boolean;
  attachments: Attachment[];
  onChange: (value: string) => void;
  onAddAttachments: (attachments: Attachment[], tabId?: string) => void;
  onRemoveAttachment: (index: number) => void;
  onSubmit: (attachments: Attachment[]) => Promise<boolean>;
  onStop: () => void;
  onCompact: () => void;
}) {
  const [helpOpen, setHelpOpen] = useState(false);
  const [attachmentError, setAttachmentError] = useState("");
  const [reading, setReading] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const activeTabIdRef = useRef(activeTabId);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const helpPopoverRef = useRef<HTMLDivElement>(null);
  const composingRef = useRef(false);
  const submittingRef = useRef(false);
  const modelLabel = formatModelId(model);
  activeTabIdRef.current = activeTabId;

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
    const tabId = activeTabIdRef.current;
    setAttachmentError("");
    setReading(true);
    try {
      const added: Attachment[] = [];
      for (const file of files) {
        try {
          added.push(await readAttachment(file));
        } catch (error) {
          if (activeTabIdRef.current === tabId) {
            setAttachmentError(errorText(error));
          }
        }
      }
      if (added.length > 0) {
        onAddAttachments(added, tabId);
      }
    } finally {
      setReading(false);
    }
  }

  async function submit() {
    if (compacting || reading || submittingRef.current) return;
    submittingRef.current = true;
    try {
      if (!await onSubmit(attachments)) return;
      setAttachmentError("");
      if (textareaRef.current) textareaRef.current.style.height = "auto";
    } finally {
      submittingRef.current = false;
    }
  }

  return (
    <div className="composer-wrap">
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
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
            void submit();
          }}
        />
        <div className="composer-footer">
          <span className="composer-meta">
            <span className="composer-context">
              {showContextGauge && (
                <ContextGauge
                  usage={context}
                  runUsage={usage}
                  running={running}
                  compacting={compacting}
                  onCompact={onCompact}
                />
              )}
              {context && (
                <span
                  className="composer-usage"
                  title={
                    stepUsage
                      ? formatRequestTitle(stepUsage)
                      : "Current context; the cache share appears after the next reply"
                  }
                >
                  <UsageLabel
                    key={activeTabId}
                    used={context.used}
                    limit={context.window}
                    step={stepUsage}
                  />
                </span>
              )}
            </span>
            <span className="composer-model" title={`Model: ${model}`}>
              <span className="sr-only">Model: </span>
              {modelLabel}
            </span>
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
                    {!welcome && (
                      <div className="shortcut">
                        <dt>
                          <kbd>⌘ N</kbd>
                        </dt>
                        <dd>New session</dd>
                      </div>
                    )}
                    {!welcome && (
                      <div className="shortcut">
                        <dt>
                          <kbd>⌘ X</kbd>
                        </dt>
                        <dd>Close tab</dd>
                      </div>
                    )}
                    <div className="shortcut">
                      <dt>
                        <kbd>⌘ R</kbd>
                      </dt>
                      <dd>Search history</dd>
                    </div>
                    {!welcome && (
                      <div className="shortcut">
                        <dt>
                          <kbd>⌘ F</kbd>
                        </dt>
                        <dd>Find in chat</dd>
                      </div>
                    )}
                    {running && (
                      <div className="shortcut">
                        <dt>
                          <kbd>⌃ C</kbd>
                        </dt>
                        <dd>Stop run</dd>
                      </div>
                    )}
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
          </span>
          {running ? (
            <button
              type="button"
              className="stop icon-button"
              onClick={onStop}
              aria-label="Stop"
              title="Stop"
            >
              <PauseFilled size={14} />
            </button>
          ) : (
            <button
              type="submit"
              className="primary icon-button"
              disabled={
                compacting ||
                reading ||
                (!value.trim() && attachments.length === 0)
              }
              aria-label={compacting ? "Compacting context" : "Send"}
              title={compacting ? "Compacting context…" : "Send"}
            >
              <SendFilled size={16} />
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
