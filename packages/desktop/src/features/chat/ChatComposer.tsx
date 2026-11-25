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
  KeyboardRegular,
} from "@mingcute/react/core-regular";
import { PauseFilled } from "@mingcute/react/core-filled";
import {
  createCompositionGuard,
  isImeEvent,
  isSendEnter,
} from "../../app/shortcuts";
import { errorText } from "../../shared/error-text";
import { ImageLightbox } from "../../shared/ImageLightbox";
import {
  formatBytes,
  readAttachment,
  type Attachment,
} from "./attachments";
import {
  activeMention,
  insertMention,
  mentionOptions,
  type ActiveMention,
  type MentionCommand,
} from "./mentions";
import {
  contextTier,
  countUpValue,
  formatContextLabel,
  formatContextUsage,
  formatModelId,
  formatRequestTitle,
  formatUsageTitle,
} from "./chat-model";
import { ContextInspector } from "./ContextInspector";
import type { ContextUsage, RunUsage, StepUsage } from "./session-model";

export type ChatComposerHandle = {
  focus: () => void;
};

type ImageAttachment = Extract<Attachment, { kind: "image" }>;

function resizeComposer(element: HTMLTextAreaElement) {
  const scrollTop = element.scrollTop;
  element.style.height = "auto";
  element.style.height = `${element.scrollHeight}px`;
  element.scrollTop = scrollTop;
}

function UsageLabel({ used, limit }: { used: number; limit: number }) {
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

  return <>{formatContextLabel(shown, limit)}</>;
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
  autoFocus,
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
  autoFocus?: boolean;
  attachments: Attachment[];
  onChange: (value: string) => void;
  onAddAttachments: (attachments: Attachment[], tabId?: string) => void;
  onRemoveAttachment: (index: number) => void;
  onSubmit: (attachments: Attachment[]) => Promise<boolean>;
  onStop: () => void;
  onCompact: () => void;
}) {
  const [helpOpen, setHelpOpen] = useState(false);
  const [usageOpen, setUsageOpen] = useState(false);
  const [attachmentError, setAttachmentError] = useState("");
  const [preview, setPreview] = useState<ImageAttachment | null>(null);
  const [reading, setReading] = useState(false);
  const [mention, setMention] = useState<ActiveMention | null>(null);
  const [mentionIndex, setMentionIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const activeTabIdRef = useRef(activeTabId);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const helpPopoverRef = useRef<HTMLDivElement>(null);
  const usageRef = useRef<HTMLSpanElement>(null);
  const compositionRef = useRef(createCompositionGuard());
  const submittingRef = useRef(false);
  const modelLabel = formatModelId(model);
  const options = mention ? mentionOptions(mention.query) : [];
  const mentionOpen = options.length > 0;
  const activeIndex =
    options.length === 0 ? 0 : Math.min(mentionIndex, options.length - 1);
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

  useEffect(() => {
    if (!usageOpen) return;
    function onMouseDown(event: MouseEvent) {
      if (!usageRef.current?.contains(event.target as Node)) {
        setUsageOpen(false);
      }
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setUsageOpen(false);
    }
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [usageOpen]);

  useLayoutEffect(() => {
    setHelpOpen(false);
    setUsageOpen(false);
    setAttachmentError("");
    setPreview(null);
  }, [activeTabId]);

  // The welcome tab has nothing else to interact with, so typing can start
  // immediately instead of requiring a click into the composer.
  useEffect(() => {
    if (!welcome || !autoFocus) return;
    textareaRef.current?.focus();
  }, [activeTabId, autoFocus, welcome]);

  useLayoutEffect(() => {
    if (textareaRef.current) resizeComposer(textareaRef.current);
  }, [activeTabId, running, value, welcome]);

  useLayoutEffect(() => {
    const element = textareaRef.current;
    if (!element) return;

    let frame = 0;
    let width: number | undefined;
    const observer = new ResizeObserver(([{ contentRect }]) => {
      if (width === contentRect.width) return;
      width = contentRect.width;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => resizeComposer(element));
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, []);

  useImperativeHandle(ref, () => ({
    focus: () => textareaRef.current?.focus(),
  }), []);

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

  function syncMention(text: string, caret: number | null) {
    if (compositionRef.current.active() || caret === null) {
      setMention(null);
      return;
    }
    const next = activeMention(text, caret);
    if (!next || mentionOptions(next.query).length === 0) {
      setMention(null);
      return;
    }
    setMention(next);
    setMentionIndex(0);
  }

  function acceptMention(command: MentionCommand) {
    if (!mention) return;
    const inserted = insertMention(value, mention, command.name);
    onChange(inserted.value);
    setMention(null);
    requestAnimationFrame(() => {
      const element = textareaRef.current;
      if (element?.value !== inserted.value) return;
      element.focus();
      element.setSelectionRange(inserted.caret, inserted.caret);
    });
  }

  async function submit() {
    if (compacting || reading || submittingRef.current) return;
    submittingRef.current = true;
    try {
      if (!await onSubmit(attachments)) return;
      setAttachmentError("");
    } finally {
      submittingRef.current = false;
    }
  }

  const gauge = showContextGauge ? (
    <ContextGauge
      usage={context}
      runUsage={usage}
      running={running}
      compacting={compacting}
      onCompact={onCompact}
    />
  ) : null;
  const stopButton = running && (
    <button
      type="button"
      className="stop icon-button"
      onClick={onStop}
      aria-label="Stop"
      title="Stop"
    >
      <PauseFilled size={16} aria-hidden="true" />
    </button>
  );

  return (
    <div className="composer-wrap" data-welcome={welcome ? "" : undefined}>
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
                  <button
                    type="button"
                    className="composer-attachment-thumb"
                    aria-label={`Preview ${attachment.name}`}
                    title="Preview image"
                    onClick={() => setPreview(attachment)}
                  >
                    <img src={attachment.thumbnail} alt="" />
                  </button>
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
        {mentionOpen && (
          <div
            className="mention-menu"
            id="composer-mentions"
            role="listbox"
            aria-label="Commands"
          >
            {options.map((option, index) => (
              <button
                key={option.name}
                type="button"
                role="option"
                id={`composer-mention-${option.name}`}
                aria-selected={index === activeIndex}
                className={`mention-option${
                  index === activeIndex ? " active" : ""
                }`}
                // Keep the textarea focused so selecting does not blur first.
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => acceptMention(option)}
              >
                <span className="mention-copy">
                  <span className="mention-name">@{option.name}</span>
                  <span className="mention-description">{option.description}</span>
                </span>
                <kbd aria-hidden="true">↵</kbd>
              </button>
            ))}
          </div>
        )}
        <div className="composer-input">
          <textarea
            ref={textareaRef}
            rows={1}
            value={value}
            placeholder={
              welcome
                ? running
                  ? "Press Enter to steer"
                  : "Press Enter to send"
                : running
                  ? "Steer the current turn…"
                  : "Message Oaklet…"
            }
            aria-label="Message"
            aria-autocomplete="list"
            aria-controls={mentionOpen ? "composer-mentions" : undefined}
            aria-expanded={mentionOpen}
            aria-activedescendant={
              mentionOpen
                ? `composer-mention-${options[activeIndex]?.name}`
                : undefined
            }
            onChange={(event) => {
              onChange(event.target.value);
              syncMention(event.target.value, event.target.selectionStart);
            }}
            onSelect={(event) =>
              syncMention(
                event.currentTarget.value,
                event.currentTarget.selectionStart,
              )
            }
            onPaste={pasteFiles}
            onCompositionStart={() => {
              compositionRef.current.start();
            }}
            onCompositionEnd={() => {
              // Safari fires compositionend before the Enter keydown that
              // confirms the candidate, so keep the flag through that keydown.
              compositionRef.current.end();
            }}
            onBlur={() => {
              compositionRef.current.reset();
              setMention(null);
            }}
            onKeyDown={(event) => {
              const composing = compositionRef.current.active();
              if (
                mentionOpen &&
                !isImeEvent(event.nativeEvent) &&
                !composing
              ) {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  const step = event.key === "ArrowDown" ? 1 : -1;
                  setMentionIndex(
                    (index) =>
                      (index + step + options.length) % options.length,
                  );
                  return;
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  setMention(null);
                  return;
                }
                if (
                  event.key === "Enter" ||
                  (event.key === "Tab" && !event.shiftKey)
                ) {
                  event.preventDefault();
                  const option = options[activeIndex] ?? options[0];
                  if (option) acceptMention(option);
                  return;
                }
              }
              if (!isSendEnter(event.nativeEvent, composing)) return;
              event.preventDefault();
              void submit();
            }}
          />
          {!welcome && stopButton}
        </div>
        <div className="composer-footer">
          <span className="composer-meta">
            {(gauge || context) && (
              <span className="composer-context" ref={usageRef}>
                {gauge}
                {context && (
                  <button
                    type="button"
                    className="composer-usage"
                    title={stepUsage ? formatRequestTitle(stepUsage) : "Current context"}
                    aria-haspopup="dialog"
                    aria-expanded={usageOpen}
                    aria-controls={usageOpen ? "context-inspector" : undefined}
                    onClick={() => setUsageOpen((open) => !open)}
                  >
                    <UsageLabel
                      key={activeTabId}
                      used={context.used}
                      limit={context.window}
                    />
                  </button>
                )}
                {usageOpen && context && (
                  <ContextInspector usage={context} />
                )}
              </span>
            )}
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
                        <kbd>⌘ V</kbd>
                      </dt>
                      <dd>Paste image or file</dd>
                    </div>
                  </dl>
                </div>
              )}
            </span>
          </span>
          {welcome && stopButton}
        </div>
      </form>
      {preview && (
        <ImageLightbox
          src={preview.dataUrl}
          alt={preview.name}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}
