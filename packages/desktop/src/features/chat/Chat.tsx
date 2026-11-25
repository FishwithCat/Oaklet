import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
  type WheelEvent,
} from "react";
import {
  AddRegular,
  CloseRegular,
  Message4Regular,
  TerminalBoxRegular,
} from "@mingcute/react/core-regular";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ChatComposer, type ChatComposerHandle } from "./ChatComposer";
import { ChatFind } from "./ChatFind";
import { ChatThread } from "./ChatThread";
import { Welcome } from "./Welcome";
import { readDroppedFile, type Attachment } from "./attachments";
import {
  isAtBottom,
  mergeDraft,
  shouldShowContextGauge,
  stickAfterScroll,
  type Item,
} from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";
import type { PlanBaseline, PlanCompareData } from "./plan-model";
import type {
  ChatTabInfo,
  ContextUsage,
  PendingInput,
  RunUsage,
  SendMode,
  StepUsage,
} from "./session-model";
import { useSelectionAction } from "./useSelectionAction";
import { useTabDrag } from "../../shared/useTabDrag";

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
  overlayOpen,
  tabs,
  activeTabId,
  onSelectTab,
  onNewTab,
  onMoveTab,
  onCloseTab,
  items,
  pending,
  firstSeq,
  hasMore,
  loadingOlder,
  loadOlderError,
  running,
  compacting,
  activeRunId,
  context,
  usage,
  stepUsage,
  model,
  onSend,
  onStop,
  onCompact,
  onLoadOlder,
  onApproval,
  onConfirmPlan,
  onCheckPlanBaseline,
  onLoadPlanCompare,
  onSendToTerminal,
  onOpenFile,
}: {
  ref?: Ref<ChatHandle>;
  active: boolean;
  overlayOpen: boolean;
  tabs: ChatTabInfo[];
  activeTabId: string;
  onSelectTab: (id: string) => void;
  onNewTab: () => void;
  onMoveTab: (from: number, to: number) => void;
  onCloseTab: (id: string) => void;
  items: Item[];
  pending: PendingInput[];
  firstSeq: number | null;
  hasMore: boolean;
  loadingOlder: boolean;
  loadOlderError: string;
  running: boolean;
  compacting: boolean;
  activeRunId: string | null;
  context: ContextUsage | null;
  usage: RunUsage | null;
  stepUsage: StepUsage | null;
  model: string;
  onSend: (
    text: string,
    attachments: Attachment[],
    mode: SendMode,
  ) => Promise<boolean>;
  onStop: () => void;
  onCompact: () => void;
  onLoadOlder: () => Promise<boolean>;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  onConfirmPlan: (sessionId: string, version: number) => Promise<boolean>;
  onCheckPlanBaseline: (sessionId: string, version: number) => Promise<PlanBaseline>;
  onLoadPlanCompare: (
    sessionId: string,
    version: number,
    viewId: string,
  ) => Promise<PlanCompareData>;
  onSendToTerminal: (text: string) => void;
  onOpenFile: (href: string) => void;
}) {
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const chatRef = useRef<HTMLElement>(null);
  const composerRef = useRef<ChatComposerHandle>(null);
  const stickToBottomRef = useRef(true);
  const lastScrollTopRef = useRef(0);
  const [historyAnchor, setHistoryAnchor] = useState<{
    height: number;
    top: number;
    firstSeq: number;
    tabId: string;
    anchorSeq: number | null;
    anchorTop: number | null;
  } | null>(null);
  const draftsRef = useRef(new Map<string, string>());
  const attachmentsRef = useRef(new Map<string, Attachment[]>());
  const lastTabIdRef = useRef(activeTabId);
  const activeTabIdRef = useRef(activeTabId);
  const activeRef = useRef(active);
  const dropOverRef = useRef(false);
  const submittingRef = useRef(false);
  const [dropping, setDropping] = useState(false);
  activeTabIdRef.current = activeTabId;
  activeRef.current = active;
  // Any blank thread is a welcome screen, including a new tab from Command+N.
  const welcome = items.length === 0 && !hasMore;
  const { selectionAction, selectionButtonRef, clearSelection } =
    useSelectionAction({ active: active && !welcome, containerRef: chatRef });
  const { draggingId, dragProps } = useTabDrag(onMoveTab);

  function pinToBottom() {
    const element = chatRef.current;
    if (!element) return;
    // Write the offset directly: scrollIntoView aligns to the padding box, so
    // the container's bottom padding stays out of reach and the pin's own
    // scroll event reads as "not at the bottom".
    element.scrollTop = element.scrollHeight;
    lastScrollTopRef.current = element.scrollTop;
  }

  useLayoutEffect(() => {
    if (lastTabIdRef.current === activeTabId) return;
    lastTabIdRef.current = activeTabId;
    setInput(draftsRef.current.get(activeTabId) ?? "");
    setAttachments(attachmentsRef.current.get(activeTabId) ?? []);
    clearSelection();
    setHistoryAnchor(null);
    stickToBottomRef.current = true;
    pinToBottom();
  }, [activeTabId]);

  // Pin before paint so a streamed chunk never flashes at the old position.
  useLayoutEffect(() => {
    if (stickToBottomRef.current) {
      pinToBottom();
    }
  }, [items, pending.length, compacting]);

  // A prepend keeps the row the user was reading under the same viewport
  // position; only a window that actually grew older consumes the anchor.
  useLayoutEffect(() => {
    if (
      historyAnchor === null ||
      historyAnchor.tabId !== activeTabId ||
      firstSeq === null ||
      firstSeq >= historyAnchor.firstSeq
    ) {
      return;
    }
    const element = chatRef.current;
    if (!element) return;
    const anchored =
      historyAnchor.anchorSeq === null
        ? null
        : element.querySelector<HTMLElement>(
            `[data-seq="${historyAnchor.anchorSeq}"]`,
          );
    const anchoredTop = anchored?.getBoundingClientRect().top ?? null;
    if (historyAnchor.anchorTop !== null && anchoredTop !== null) {
      element.scrollTop += anchoredTop - historyAnchor.anchorTop;
    } else {
      element.scrollTop =
        element.scrollHeight - historyAnchor.height + historyAnchor.top;
    }
    lastScrollTopRef.current = element.scrollTop;
    stickToBottomRef.current = false;
    setHistoryAnchor(null);
  }, [activeTabId, firstSeq, historyAnchor, items]);

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
    const top = element.scrollTop;
    stickToBottomRef.current = stickAfterScroll(
      stickToBottomRef.current,
      isAtBottom(element),
      top,
      lastScrollTopRef.current,
    );
    lastScrollTopRef.current = top;
    if (selectionAction) clearSelection();
  }

  function onWheel(event: WheelEvent<HTMLElement>) {
    const element = chatRef.current;
    // The wheel is the only synchronous signal that the user is scrolling up.
    // Waiting for onScroll loses the race against the per-chunk pin, which
    // snaps the thread back before the scroll event can detach it.
    if (event.deltaY < 0 && element && element.scrollTop > 0) {
      stickToBottomRef.current = false;
    }
  }

  function updateDraft(value: string) {
    draftsRef.current.set(activeTabId, value);
    setInput(value);
  }

  function updateAttachments(next: Attachment[]) {
    attachmentsRef.current.set(activeTabId, next);
    setAttachments(next);
  }

  function addAttachments(added: Attachment[], tabId = activeTabId) {
    const next = [...(attachmentsRef.current.get(tabId) ?? []), ...added];
    attachmentsRef.current.set(tabId, next);
    if (activeTabIdRef.current === tabId) setAttachments(next);
  }

  function appendDraft(addition: string, tabId = activeTabId) {
    const current = draftsRef.current.get(tabId) ?? "";
    const draft = current.trimEnd();
    const next = draft ? `${draft} ${addition}` : addition;
    draftsRef.current.set(tabId, next);
    if (activeTabIdRef.current === tabId) setInput(next);
  }

  function isChatDropPosition(position: { x: number; y: number }): boolean {
    if (!activeRef.current) return false;
    const scale = window.devicePixelRatio || 1;
    const target = document.elementFromPoint(
      position.x / scale,
      position.y / scale,
    );
    return Boolean(target?.closest(".main"));
  }

  function setDropOver(next: boolean) {
    if (dropOverRef.current === next) return;
    dropOverRef.current = next;
    setDropping(next);
  }

  async function addDroppedPaths(paths: string[]) {
    const tabId = activeTabIdRef.current;
    const added: Attachment[] = [];
    const fallback: string[] = [];
    for (const path of paths) {
      try {
        const attachment = await readDroppedFile(path);
        if (attachment) added.push(attachment);
        else fallback.push(path);
      } catch {
        // An unreadable file is still useful to the model as a path.
        fallback.push(path);
      }
    }
    if (added.length > 0) addAttachments(added, tabId);
    if (fallback.length > 0) appendDraft(fallback.join(" "), tabId);
    if (activeTabIdRef.current === tabId) {
      requestAnimationFrame(() => composerRef.current?.focus());
    }
  }

  useEffect(() => {
    let disposed = false;
    let release: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (disposed) return;
        const payload = event.payload;
        if (payload.type === "over" || payload.type === "enter") {
          setDropOver(isChatDropPosition(payload.position));
          return;
        }
        setDropOver(false);
        if (payload.type === "drop" && isChatDropPosition(payload.position)) {
          void addDroppedPaths(payload.paths);
        }
      })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((error) => {
        console.error(`Could not watch for dropped files: ${String(error)}`);
      });
    return () => {
      disposed = true;
      release?.();
    };
  }, []);

  async function submit(attachments: Attachment[]): Promise<boolean> {
    if (submittingRef.current) return false;
    submittingRef.current = true;
    const tabId = activeTabIdRef.current;
    const text = input;
    try {
      const accepted = await onSend(
        text,
        attachments,
        running ? "steer" : "normal",
      );
      if (!accepted) return false;
      // A successful send consumes this tab's draft, but do not erase text
      // typed in another tab while the request was in flight.
      if (draftsRef.current.get(tabId) === text) {
        draftsRef.current.delete(tabId);
      }
      if (attachmentsRef.current.get(tabId) === attachments) {
        attachmentsRef.current.delete(tabId);
      }
      if (activeTabIdRef.current === tabId) {
        setInput("");
        setAttachments([]);
        stickToBottomRef.current = true;
      }
      return true;
    } finally {
      submittingRef.current = false;
    }
  }

  async function loadEarlier() {
    const element = chatRef.current;
    if (
      !element ||
      !hasMore ||
      loadingOlder ||
      compacting ||
      firstSeq === null ||
      historyAnchor !== null
    ) {
      return;
    }
    stickToBottomRef.current = false;
    const first = element.querySelector<HTMLElement>("[data-seq]");
    const anchorSeq =
      first?.dataset.seq === undefined ? null : Number(first.dataset.seq);
    setHistoryAnchor({
      height: element.scrollHeight,
      top: element.scrollTop,
      firstSeq,
      tabId: activeTabId,
      anchorSeq,
      anchorTop: first?.getBoundingClientRect().top ?? null,
    });
    if (!(await onLoadOlder())) setHistoryAnchor(null);
  }

  const showTabs = items.length > 0 || tabs.length > 1;
  const composer = active && (
    <ChatComposer
      ref={composerRef}
      activeTabId={activeTabId}
      value={input}
      running={running}
      compacting={compacting}
      context={context}
      usage={usage}
      stepUsage={stepUsage}
      showContextGauge={shouldShowContextGauge(context, items.length > 0)}
      model={model}
      welcome={welcome}
      autoFocus={!overlayOpen}
      attachments={attachments}
      onChange={updateDraft}
      onAddAttachments={addAttachments}
      onRemoveAttachment={(index) =>
        updateAttachments(
          attachments.filter((_, position) => position !== index),
        )
      }
      onSubmit={submit}
      onStop={onStop}
      onCompact={onCompact}
    />
  );
  const dropHint = dropping && (
    <div className="chat-drop" role="status">
      <span>Drop files to attach — large files insert their path</span>
    </div>
  );

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
              }${tab.id === draggingId ? " dragging" : ""}`}
              {...dragProps(tab.id)}
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
        onWheel={onWheel}
      >
        {hasMore && (
          <div className="load-older">
            <button
              type="button"
              disabled={loadingOlder || compacting}
              onClick={loadEarlier}
            >
              {loadingOlder ? "Loading earlier…" : "Load earlier"}
            </button>
            {loadOlderError && (
              <p className="load-older-error" role="alert">
                {loadOlderError}
              </p>
            )}
          </div>
        )}
        {items.length === 0 ? (
          <Welcome />
        ) : (
          <ChatThread
            sessionId={activeTabId}
            items={items}
            pending={pending}
            running={running}
            compacting={compacting}
            activeRunId={activeRunId}
            onApproval={onApproval}
            onConfirmPlan={(version) => onConfirmPlan(activeTabId, version)}
            onRevisePlan={() => composerRef.current?.focus()}
            onCheckPlanBaseline={(version) => onCheckPlanBaseline(activeTabId, version)}
            onLoadPlanCompare={(version, viewId) => onLoadPlanCompare(activeTabId, version, viewId)}
            onOpenFile={onOpenFile}
          />
        )}
      </main>

      {!welcome && (
        <ChatFind
          active={active}
          tabId={activeTabId}
          containerRef={chatRef}
          items={items}
        />
      )}

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

      {composer}
      {dropHint}
    </>
  );
}
