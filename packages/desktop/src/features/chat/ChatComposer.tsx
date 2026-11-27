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
import { formatModelId } from "./chat-model";
import { ComposerContext } from "./ComposerContext";
import { ComposerShortcuts } from "./ComposerShortcuts";
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
  const [attachmentError, setAttachmentError] = useState("");
  const [preview, setPreview] = useState<ImageAttachment | null>(null);
  const [reading, setReading] = useState(false);
  const [mention, setMention] = useState<ActiveMention | null>(null);
  const [mentionIndex, setMentionIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const activeTabIdRef = useRef(activeTabId);
  const compositionRef = useRef(createCompositionGuard());
  const submittingRef = useRef(new Set<string>());
  const modelLabel = formatModelId(model);
  const options = mention ? mentionOptions(mention.query) : [];
  const mentionOpen = options.length > 0;
  const activeIndex =
    options.length === 0 ? 0 : Math.min(mentionIndex, options.length - 1);
  activeTabIdRef.current = activeTabId;

  useLayoutEffect(() => {
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
    if (reading || submittingRef.current.has(activeTabId)) return;
    submittingRef.current.add(activeTabId);
    try {
      if (!await onSubmit(attachments)) return;
      setAttachmentError("");
    } finally {
      submittingRef.current.delete(activeTabId);
    }
  }

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
                  : compacting
                    ? "Message waits for compaction…"
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
            <ComposerContext
              activeTabId={activeTabId}
              context={context}
              usage={usage}
              stepUsage={stepUsage}
              running={running}
              compacting={compacting}
              showContextGauge={showContextGauge}
              onCompact={onCompact}
            />
            <span className="composer-model" title={`Model: ${model}`}>
              <span className="sr-only">Model: </span>
              {modelLabel}
            </span>
            <ComposerShortcuts
              activeTabId={activeTabId}
              running={running}
              welcome={welcome}
            />
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
