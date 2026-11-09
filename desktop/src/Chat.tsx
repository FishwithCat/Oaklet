import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Ref,
} from "react";
import {
  PaperPlaneRightIcon,
  QuestionIcon,
  TerminalWindowIcon,
} from "@phosphor-icons/react";
import { MarkdownText } from "./Markdown";
import { ProcessGroup } from "./Process";
import { mergeDraft } from "./architecture-prompt";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";

export type ChatHandle = {
  reset: () => void;
  prefill: (prompt: string) => void;
};

export function Chat({
  ref,
  active,
  items,
  running,
  activeRunId,
  onSend,
  onApproval,
  onSendToTerminal,
}: {
  ref?: Ref<ChatHandle>;
  active: boolean;
  items: Item[];
  running: boolean;
  activeRunId: string | null;
  onSend: (text: string) => boolean;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  onSendToTerminal: (text: string) => void;
}) {
  const [input, setInput] = useState("");
  const [helpOpen, setHelpOpen] = useState(false);
  const [selectionAction, setSelectionAction] = useState<{
    text: string;
    x: number;
    y: number;
    above: boolean;
  } | null>(null);
  const chatRef = useRef<HTMLElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const helpButtonRef = useRef<HTMLButtonElement>(null);
  const helpPopoverRef = useRef<HTMLDivElement>(null);
  const selectionButtonRef = useRef<HTMLButtonElement>(null);
  const stickToBottomRef = useRef(true);

  useEffect(() => {
    if (!active) setHelpOpen(false);
  }, [active]);

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

  useEffect(() => {
    if (stickToBottomRef.current) {
      endRef.current?.scrollIntoView({ block: "end" });
    }
  }, [items]);

  useEffect(() => {
    if (!active) {
      setSelectionAction(null);
      return;
    }
    let frame = 0;

    function showSelection() {
      const root = chatRef.current;
      const selection = window.getSelection();
      if (
        !root ||
        !selection ||
        selection.isCollapsed ||
        selection.rangeCount === 0
      ) {
        setSelectionAction(null);
        return;
      }
      const text = selection.toString().trim();
      const range = selection.getRangeAt(0);
      if (!text || !root.contains(range.commonAncestorContainer)) {
        setSelectionAction(null);
        return;
      }
      const rect = range.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      setSelectionAction({
        text,
        x: Math.min(Math.max(x, 80), window.innerWidth - 80),
        y: rect.top > 48 ? rect.top : rect.bottom,
        above: rect.top > 48,
      });
    }

    function scheduleSelection() {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(showSelection);
    }

    function onRelease(event: Event) {
      const target = event.target;
      if (
        target instanceof Node &&
        selectionButtonRef.current?.contains(target)
      ) {
        return;
      }
      scheduleSelection();
    }

    function hideSelection(event?: Event) {
      const target = event?.target;
      if (
        target instanceof Node &&
        selectionButtonRef.current?.contains(target)
      ) {
        return;
      }
      cancelAnimationFrame(frame);
      setSelectionAction(null);
    }

    function onSelectionChange() {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) hideSelection();
    }

    document.addEventListener("mousedown", hideSelection);
    document.addEventListener("keydown", hideSelection);
    document.addEventListener("selectstart", hideSelection);
    document.addEventListener("mouseup", onRelease);
    document.addEventListener("keyup", onRelease);
    document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("mousedown", hideSelection);
      document.removeEventListener("keydown", hideSelection);
      document.removeEventListener("selectstart", hideSelection);
      document.removeEventListener("mouseup", onRelease);
      document.removeEventListener("keyup", onRelease);
      document.removeEventListener("selectionchange", onSelectionChange);
    };
  }, [active]);

  useImperativeHandle(
    ref,
    () => ({
      reset: () => {
        setInput("");
        setHelpOpen(false);
        if (textareaRef.current) textareaRef.current.style.height = "auto";
        textareaRef.current?.focus();
      },
      prefill: (prompt: string) => {
        setInput((current) => mergeDraft(current, prompt));
        requestAnimationFrame(() => {
          resizeComposer();
          textareaRef.current?.focus();
        });
      },
    }),
    [],
  );

  function onScroll() {
    const element = chatRef.current;
    if (!element) return;
    stickToBottomRef.current =
      element.scrollHeight - element.scrollTop - element.clientHeight < 80;
    if (selectionAction) setSelectionAction(null);
  }

  function resizeComposer() {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 180)}px`;
  }

  function submit() {
    if (!onSend(input)) return;
    setInput("");
    stickToBottomRef.current = true;
    if (textareaRef.current) textareaRef.current.style.height = "auto";
  }

  const blocks = toBlocks(items);
  let lastProcessBlock = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    if (blocks[index]?.kind === "process") {
      lastProcessBlock = index;
      break;
    }
  }

  return (
    <>
      <main
        className="chat"
        hidden={!active}
        ref={chatRef}
        onScroll={onScroll}
      >
        {items.length === 0 ? (
          <div className="empty">
            <h1>What should we build?</h1>
          </div>
        ) : (
          <div className="thread" aria-live="polite">
            {blocks.map((block, blockIndex) => {
              if (block.kind === "process") {
                return (
                  <ProcessGroup
                    key={`process-${block.start}`}
                    items={block.items}
                    live={
                      running &&
                      blockIndex === lastProcessBlock &&
                      block.items[0]?.runId === activeRunId
                    }
                    onApproval={onApproval}
                  />
                );
              }

              const item = block.item;
              if (item.kind === "user") {
                return (
                  <div className="msg user" key={`user-${block.index}`}>
                    <div className="bubble">{item.text}</div>
                  </div>
                );
              }
              if (item.kind === "assistant") {
                return (
                  <div
                    className="msg assistant"
                    key={`assistant-${block.index}`}
                  >
                    <div className="bubble">
                      <MarkdownText text={item.text} />
                      {item.streaming && <span className="caret" />}
                    </div>
                  </div>
                );
              }
              if (item.kind !== "notice") return null;
              return (
                <div
                  className={`notice ${item.tone}`}
                  key={`notice-${block.index}`}
                >
                  {item.text}
                </div>
              );
            })}
            <div ref={endRef} />
          </div>
        )}
      </main>

      {active && selectionAction && (
        <button
          ref={selectionButtonRef}
          type="button"
          className={`selection-action${
            selectionAction.above ? "" : " below"
          }`}
          style={{ left: selectionAction.x, top: selectionAction.y }}
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            onSendToTerminal(selectionAction.text);
            setSelectionAction(null);
          }}
          title="Run the selected text in the terminal"
        >
          <TerminalWindowIcon size={13} />
          Send to Terminal
        </button>
      )}

      {active && (
        <div className="composer-wrap">
          <form
            className="composer"
            onSubmit={(event) => {
              event.preventDefault();
              submit();
            }}
          >
            <textarea
              ref={textareaRef}
              rows={1}
              value={input}
              placeholder="Ask Oaklet to build, fix, or explain something..."
              onChange={(event) => {
                setInput(event.target.value);
                resizeComposer();
              }}
              onKeyDown={(event) => {
                if (
                  event.key === "Enter" &&
                  !event.shiftKey &&
                  !event.nativeEvent.isComposing
                ) {
                  event.preventDefault();
                  submit();
                }
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
                disabled={running || !input.trim()}
                aria-label="Send"
                title="Send"
              >
                <PaperPlaneRightIcon size={16} weight="fill" />
              </button>
            </div>
          </form>
        </div>
      )}
    </>
  );
}
