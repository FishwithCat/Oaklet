import type { ReactNode } from "react";
import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
  type WheelEvent,
} from "react";
import { TerminalBoxRegular } from "@mingcute/react/core-regular";
import { ArrowBigUpDashFilled } from "@mingcute/react/core-filled";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ChatComposer, type ChatComposerHandle } from "./ChatComposer";
import { ChatFind } from "./ChatFind";
import { ChatTabs } from "./ChatTabs";
import { ChatThread, PendingMessages } from "./ChatThread";
import { Welcome } from "./Welcome";
import { quotePaths, readDroppedFile, type Attachment } from "./attachments";
import type { WorktreeInfo } from "./WorktreeControl";
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
import { SelectionActionButton } from "../../shared/SelectionActionButton";
import { useSelectionAction } from "../../shared/useSelectionAction";

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
  onRemovePending,
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
  checkoutControl,
  worktrees,
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
  onRemovePending: (sessionId: string, pendingId: string) => void;
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
    retryWorktree?: boolean,
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
  checkoutControl?: ((retry: (keepRequest: boolean, text: string) => void) => ReactNode) | null;
  worktrees?: Record<string, WorktreeInfo>;
}) {
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [scrolled, setScrolled] = useState(false);
  const chatPaneRef = useRef<HTMLElement>(null);
  const chatRef = useRef<HTMLDivElement>(null);
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
  const submittingRef = useRef(new Set<string>());
  const [dropping, setDropping] = useState(false);
  activeTabIdRef.current = activeTabId;
  activeRef.current = active;
  // Any blank thread is a welcome screen, including a new tab from Command+N.
  const welcome = items.length === 0 && !hasMore;
  const selection = useSelectionAction({ active: active && !welcome, containerRef: chatPaneRef });

  function pinToBottom() {
    const element = chatRef.current;
    if (!element) return;
    // Write the offset directly: scrollIntoView aligns to the padding box, so
    // the container's bottom padding stays out of reach and the pin's own
    // scroll event reads as "not at the bottom".
    element.scrollTop = element.scrollHeight;
    lastScrollTopRef.current = element.scrollTop;
    setScrolled(element.scrollTop > 0);
  }

  useLayoutEffect(() => {
    if (lastTabIdRef.current === activeTabId) return;
    lastTabIdRef.current = activeTabId;
    setInput(draftsRef.current.get(activeTabId) ?? "");
    setAttachments(attachmentsRef.current.get(activeTabId) ?? []);
    selection.clearSelection();
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
    setScrolled(top > 0);
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

  function jumpToLastUserMessage() {
    const element = chatRef.current;
    if (!element) return;
    const anchors = element.querySelectorAll<HTMLElement>(".msg.user");
    const target = anchors[anchors.length - 1];
    if (!target) return;
    stickToBottomRef.current = false;
    const top =
      target.getBoundingClientRect().top -
      element.getBoundingClientRect().top +
      element.scrollTop;
    // Keep the message in the thread's top padding instead of flush with the
    // pane edge, and clamp at the start for the first message.
    const inset = Number.parseFloat(getComputedStyle(element).paddingTop) || 0;
    element.scrollTo({ top: Math.max(0, top - inset), behavior: "smooth" });
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
    if (fallback.length > 0) appendDraft(quotePaths(fallback), tabId);
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

  async function submit(attachments: Attachment[], retryWorktree?: boolean, textOverride?: string): Promise<boolean> {
    const tabId = activeTabIdRef.current;
    if (submittingRef.current.has(tabId)) return false;
    submittingRef.current.add(tabId);
    const text = textOverride ?? input;
    try {
      const accepted = await onSend(
        text,
        attachments,
        running || compacting ? "steer" : "normal",
        retryWorktree,
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
      submittingRef.current.delete(tabId);
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
      <ChatTabs
        hidden={!active || !showTabs}
        tabs={tabs}
        activeTabId={activeTabId}
        onSelectTab={(id) => {
          onSelectTab(id);
          requestAnimationFrame(() => composerRef.current?.focus());
        }}
        onNewTab={onNewTab}
        onMoveTab={onMoveTab}
        onCloseTab={onCloseTab}
        worktrees={worktrees}
      />

      <main className="chat-pane" hidden={!active} ref={chatPaneRef}>
        <div className="chat" ref={chatRef} onScroll={onScroll} onWheel={onWheel}>
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
        </div>
        {scrolled && (
          <button
            type="button"
            className="icon-button chat-jump"
            aria-label="Back to your last message"
            title="Back to your last message"
            onClick={jumpToLastUserMessage}
          >
            <ArrowBigUpDashFilled size={16} aria-hidden="true" />
          </button>
        )}
      </main>

      {active && pending.length > 0 && (
        <PendingMessages
          key={activeTabId}
          sessionId={activeTabId}
          pending={pending}
          onRemovePending={onRemovePending}
        />
      )}

      {!welcome && (
        <ChatFind
          active={active}
          tabId={activeTabId}
          containerRef={chatRef}
          items={items}
        />
      )}

      {active && (
        <SelectionActionButton selection={selection} onAction={onSendToTerminal} title="Run the selected text in the terminal">
          <TerminalBoxRegular size={13} aria-hidden="true" />
          Send to Terminal
        </SelectionActionButton>
      )}

      {active && checkoutControl?.((keepRequest, text) => { void submit(attachments, keepRequest, text); })}
      {composer}
      {dropHint}
    </>
  );
}
