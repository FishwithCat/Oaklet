import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
} from "react";
import {
  AddRegular,
  CloseRegular,
  Message4Regular,
  TerminalBoxRegular,
} from "@mingcute/react/core-regular";
import { ChatComposer, type ChatComposerHandle } from "./ChatComposer";
import { ChatFind } from "./ChatFind";
import { ChatThread } from "./ChatThread";
import type { Attachment } from "./attachments";
import { mergeDraft, shouldShowContextGauge, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";
import type { ChatTabInfo, ContextUsage } from "./session-model";
import { useSelectionAction } from "./useSelectionAction";
import oakletIcon from "../../assets/oaklet-icon.png";

const STATUS_LABEL: Record<ChatTabInfo["status"], string> = {
  working: "Working",
  idle: "Idle",
  done: "Done",
  block: "Blocked",
};

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
  compacting,
  activeRunId,
  context,
  onSend,
  onCompact,
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
  compacting: boolean;
  activeRunId: string | null;
  context: ContextUsage | null;
  onSend: (text: string, attachments: Attachment[]) => boolean;
  onCompact: () => void;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  onSendToTerminal: (text: string) => void;
}) {
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const chatRef = useRef<HTMLElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<ChatComposerHandle>(null);
  const stickToBottomRef = useRef(true);
  const draftsRef = useRef(new Map<string, string>());
  const attachmentsRef = useRef(new Map<string, Attachment[]>());
  const lastTabIdRef = useRef(activeTabId);
  const { selectionAction, selectionButtonRef, clearSelection } =
    useSelectionAction({ active, containerRef: chatRef });

  useLayoutEffect(() => {
    if (lastTabIdRef.current === activeTabId) return;
    lastTabIdRef.current = activeTabId;
    setInput(draftsRef.current.get(activeTabId) ?? "");
    setAttachments(attachmentsRef.current.get(activeTabId) ?? []);
    clearSelection();
    stickToBottomRef.current = true;
    endRef.current?.scrollIntoView({ block: "end" });
  }, [activeTabId]);

  useEffect(() => {
    if (stickToBottomRef.current) {
      endRef.current?.scrollIntoView({ block: "end" });
    }
  }, [items, compacting]);

  useImperativeHandle(
    ref,
    () => ({
      prefill: (prompt: string) => {
        const next = mergeDraft(input, prompt);
        draftsRef.current.set(activeTabId, next);
        setInput(next);
        requestAnimationFrame(() => composerRef.current?.focus());
      },
    }),
    [activeTabId, input],
  );

  function onScroll() {
    const element = chatRef.current;
    if (!element) return;
    stickToBottomRef.current =
      element.scrollHeight - element.scrollTop - element.clientHeight < 80;
    if (selectionAction) clearSelection();
  }

  function updateDraft(value: string) {
    draftsRef.current.set(activeTabId, value);
    setInput(value);
  }

  function updateAttachments(next: Attachment[]) {
    attachmentsRef.current.set(activeTabId, next);
    setAttachments(next);
  }

  function addAttachments(added: Attachment[]) {
    updateAttachments([
      ...(attachmentsRef.current.get(activeTabId) ?? []),
      ...added,
    ]);
  }

  function submit(attachments: Attachment[]): boolean {
    if (!onSend(input, attachments)) return false;
    updateDraft("");
    updateAttachments([]);
    stickToBottomRef.current = true;
    return true;
  }

  const showTabs = items.length > 0 || tabs.length > 1;

  return (
    <>
      <header className="view-head" hidden={!active || !showTabs}>
        <span className="view-icon" aria-hidden="true">
          <Message4Regular size={14} />
        </span>
        <div className="view-tabs" role="tablist" aria-label="Chats">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`view-tab chat-tab${
                tab.id === activeTabId ? " active" : ""
              }`}
            >
              <span
                className={`tab-status ${tab.status}`}
                role="img"
                aria-label={`Status: ${STATUS_LABEL[tab.status]}`}
                title={STATUS_LABEL[tab.status]}
              />
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === activeTabId}
                className="view-tab-label"
                title={tab.title}
                onClick={() => {
                  onSelectTab(tab.id);
                  requestAnimationFrame(() => composerRef.current?.focus());
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
                <CloseRegular size={11} />
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
          <AddRegular size={14} />
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
            <img
              className="empty-logo"
              src={oakletIcon}
              alt="Oaklet"
              width={76}
              height={76}
            />
            <h1>What should we build?</h1>
          </div>
        ) : (
          <ChatThread
            items={items}
            running={running}
            compacting={compacting}
            activeRunId={activeRunId}
            onApproval={onApproval}
            endRef={endRef}
          />
        )}
      </main>

      <ChatFind
        active={active}
        tabId={activeTabId}
        containerRef={chatRef}
        items={items}
      />

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
            clearSelection();
          }}
          title="Run the selected text in the terminal"
        >
          <TerminalBoxRegular size={13} />
          Send to Terminal
        </button>
      )}

      {active && (
        <ChatComposer
          ref={composerRef}
          activeTabId={activeTabId}
          value={input}
          running={running}
          compacting={compacting}
          context={context}
          showContextGauge={shouldShowContextGauge(context, items.length > 0)}
          attachments={attachments}
          onChange={updateDraft}
          onAddAttachments={addAttachments}
          onRemoveAttachment={(index) =>
            updateAttachments(
              attachments.filter((_, position) => position !== index),
            )
          }
          onSubmit={submit}
          onCompact={onCompact}
        />
      )}
    </>
  );
}
