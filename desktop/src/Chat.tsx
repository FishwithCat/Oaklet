import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
} from "react";
import {
  ChatDotsIcon,
  PaperPlaneRightIcon,
  PlusIcon,
  QuestionIcon,
  TerminalWindowIcon,
  XIcon,
} from "@phosphor-icons/react";
import { MarkdownText } from "./Markdown";
import { ProcessGroup } from "./Process";
import { mergeDraft } from "./architecture-prompt";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";
import type { ChatTabInfo } from "./useAgentSession";

export type ChatHandle = {
  prefill: (prompt: string) => void;
};

export function Chat({
  ref,
  active,
  tabs,
  activeTabId,
  onSelectTab,
  onNewTab,
  onCloseTab,
  items,
  running,
  activeRunId,
  onSend,
  onApproval,
  onSendToTerminal,
}: {
  ref?: Ref<ChatHandle>;
  active: boolean;
  tabs: ChatTabInfo[];
  activeTabId: string;
  onSelectTab: (id: string) => void;
  onNewTab: () => void;
  onCloseTab: (id: string) => void;
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
  const draftsRef = useRef(new Map<string, string>());
  const lastTabIdRef = useRef(activeTabId);

  useEffect(() => {
    if (!active) setHelpOpen(false);
  }, [active]);

  useLayoutEffect(() => {
    if (lastTabIdRef.current === activeTabId) return;
    lastTabIdRef.current = activeTabId;
    setInput(draftsRef.current.get(activeTabId) ?? "");
    setHelpOpen(false);
    setSelectionAction(null);
    stickToBottomRef.current = true;
    endRef.current?.scrollIntoView({ block: "end" });
  }, [activeTabId]);

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

  useLayoutEffect(() => {
    resizeComposer();
  }, [active, activeTabId, input]);

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
      prefill: (prompt: string) => {
        const next = mergeDraft(input, prompt);
        draftsRef.current.set(activeTabId, next);
        setInput(next);
        requestAnimationFrame(() => {
          resizeComposer();
          textareaRef.current?.focus();
        });
      },
    }),
    [activeTabId, input],
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

  function updateDraft(value: string) {
    draftsRef.current.set(activeTabId, value);
    setInput(value);
  }

  function submit() {
    if (!onSend(input)) return;
    updateDraft("");
    stickToBottomRef.current = true;
    if (textareaRef.current) textareaRef.current.style.height = "auto";
  }

  const blocks = toBlocks(items);
  const showTabs = items.length > 0 || tabs.length > 1;
  let lastProcessBlock = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    if (blocks[index]?.kind === "process") {
      lastProcessBlock = index;
      break;
    }
  }

  return (
    <>
      <header className="view-head" hidden={!active || !showTabs}>
        <span className="view-icon" aria-hidden="true">
          <ChatDotsIcon size={14} />
        </span>
        <div className="view-tabs" role="tablist" aria-label="Chats">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`view-tab chat-tab${
                tab.id === activeTabId ? " active" : ""
              }`}
            >
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === activeTabId}
                className="view-tab-label"
                title={tab.title}
                onClick={() => {
                  onSelectTab(tab.id);
                  requestAnimationFrame(() => textareaRef.current?.focus());
                }}
              >
                {tab.title}
              </button>
              <button
                type="button"
                className="view-tab-close"
                onClick={() => onCloseTab(tab.id)}
                disabled={tab.running}
                aria-label={`Close ${tab.title}`}
                title={
                  tab.running
                    ? "Stop the run before closing"
                    : "Close chat"
                }
              >
                <XIcon size={11} />
              </button>
            </div>
          ))}
        </div>
        <button
          type="button"
          className="ghost icon-button view-add"
          onClick={onNewTab}
          aria-label="New chat"
          title="New chat"
        >
          <PlusIcon size={14} />
        </button>
      </header>

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
                updateDraft(event.target.value);
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
