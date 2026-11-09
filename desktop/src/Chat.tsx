import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Ref,
} from "react";
import {
  PaperPlaneRightIcon,
  StopIcon,
} from "@phosphor-icons/react";
import { MarkdownText } from "./Markdown";
import { ProcessGroup } from "./Process";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";

export type ChatHandle = {
  reset: () => void;
};

const SUGGESTIONS = [
  {
    title: "Map this codebase",
    prompt:
      "Give me a tour of this project: what it does, how it is structured, and where I should start.",
  },
  {
    title: "Fix a bug",
    prompt:
      "Find the most likely bug in this project and fix it. Explain the root cause and how to verify the fix.",
  },
  {
    title: "Add a feature",
    prompt:
      "Add a small, useful feature to this project. Keep the diff minimal and verify it.",
  },
];

export function Chat({
  ref,
  active,
  items,
  running,
  activeRunId,
  workspace,
  allowAll,
  onSend,
  onStop,
  onApproval,
}: {
  ref?: Ref<ChatHandle>;
  active: boolean;
  items: Item[];
  running: boolean;
  activeRunId: string | null;
  workspace: string;
  allowAll: boolean;
  onSend: (text: string) => boolean;
  onStop: () => void;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
}) {
  const [input, setInput] = useState("");
  const chatRef = useRef<HTMLElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const stickToBottomRef = useRef(true);

  useEffect(() => {
    if (stickToBottomRef.current) {
      endRef.current?.scrollIntoView({ block: "end" });
    }
  }, [items]);

  useImperativeHandle(
    ref,
    () => ({
      reset: () => {
        setInput("");
        if (textareaRef.current) textareaRef.current.style.height = "auto";
        textareaRef.current?.focus();
      },
    }),
    [],
  );

  function onScroll() {
    const element = chatRef.current;
    if (!element) return;
    stickToBottomRef.current =
      element.scrollHeight - element.scrollTop - element.clientHeight < 80;
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
            <div className="empty-mark">O</div>
            <h1>What should we build?</h1>
            <p>
              Oaklet reads and edits files in <code>{workspace}</code> and{" "}
              {allowAll
                ? "runs commands without asking until you revoke it."
                : "asks before running any command."}
            </p>
            <div className="suggestions">
              {SUGGESTIONS.map((suggestion) => (
                <button
                  type="button"
                  key={suggestion.title}
                  className="suggestion"
                  onClick={() => {
                    setInput(suggestion.prompt);
                    requestAnimationFrame(() => {
                      resizeComposer();
                      textareaRef.current?.focus();
                    });
                  }}
                >
                  <strong>{suggestion.title}</strong>
                  <span>{suggestion.prompt}</span>
                </button>
              ))}
            </div>
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
              <span className="hint">
                Enter to send · Shift+Enter for a new line
              </span>
              {running ? (
                <button
                  type="button"
                  className="stop icon-button"
                  onClick={onStop}
                  aria-label="Stop"
                  title="Stop"
                >
                  <StopIcon size={14} weight="fill" />
                </button>
              ) : (
                <button
                  type="submit"
                  className="primary icon-button"
                  disabled={!input.trim()}
                  aria-label="Send"
                  title="Send"
                >
                  <PaperPlaneRightIcon size={16} weight="fill" />
                </button>
              )}
            </div>
          </form>
        </div>
      )}
    </>
  );
}
