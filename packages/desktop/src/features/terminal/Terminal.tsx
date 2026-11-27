import { useImperativeHandle, useRef, useState, type Ref } from "react";
import { AddRegular, CloseRegular, TerminalBoxRegular } from "@mingcute/react/core-regular";
import { createCompositionGuard, isImeEvent } from "../../app/shortcuts";
import { QuoteToChatButton } from "../../shared/QuoteToChatButton";
import { quoteExcerpt } from "../../shared/chat-quote";
import { moveItem, useTabDrag } from "../../shared/useTabDrag";
import {
  newSessionId,
  TerminalPane,
  type TerminalPaneHandle,
} from "./TerminalPane";

export type TerminalHandle = {
  run: (command: string) => void;
  addTab: () => void;
};

type Tab = { id: string; title: string };

export function Terminal({
  ref,
  active,
  workspace,
  onClose,
  onQuote,
}: {
  ref?: Ref<TerminalHandle>;
  active: boolean;
  workspace: string;
  onClose: () => void;
  onQuote?: (quote: string) => void;
}) {
  const [tabs, setTabs] = useState<Tab[]>(() => [
    { id: newSessionId(), title: "Terminal 1" },
  ]);
  const [selected, setSelected] = useState<string | null>(null);
  const [selections, setSelections] = useState<Record<string, boolean>>({});
  const [editing, setEditing] = useState<{ id: string; width: number } | null>(null);
  const [composition] = useState(createCompositionGuard);
  const handles = useRef(new Map<string, TerminalPaneHandle>());
  const count = useRef(1);
  const { draggingId, dragProps } = useTabDrag((from, to) =>
    setTabs((current) => moveItem(current, from, to)),
  );

  const activeId = selected ?? tabs[0]?.id ?? null;

  useImperativeHandle(
    ref,
    () => ({
      run: (command) => {
        if (activeId) handles.current.get(activeId)?.run(command);
      },
      addTab,
    }),
    [activeId],
  );

  function addTab() {
    count.current += 1;
    const tab = { id: newSessionId(), title: `Terminal ${count.current}` };
    setTabs((current) => [...current, tab]);
    setSelected(tab.id);
  }

  function beginRename(id: string, label: HTMLButtonElement) {
    setEditing({ id, width: label.getBoundingClientRect().width });
  }

  function closeTab(id: string) {
    const index = tabs.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    setSelections(({ [id]: _, ...remaining }) => remaining);

    const remaining = tabs.filter((tab) => tab.id !== id);
    if (remaining.length === 0) {
      count.current += 1;
      setTabs([{ id: newSessionId(), title: `Terminal ${count.current}` }]);
      setSelected(null);
      onClose();
      return;
    }

    setTabs(remaining);
    if (activeId === id) {
      setSelected((remaining[index] ?? remaining[index - 1]).id);
    }
  }

  return (
    <section className="terminal" hidden={!active} aria-label="Terminal">
      <header className="view-bar tab-bar">
        <span className="view-bar-icon" aria-hidden="true">
          <TerminalBoxRegular size={14} />
        </span>
        <div className="view-tabs" role="tablist" aria-label="Terminals">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`view-tab${tab.id === activeId ? " active" : ""}${
                tab.id === draggingId ? " dragging" : ""
              }`}
              {...(tab.id !== editing?.id ? dragProps(tab.id) : {})}
            >
              {tab.id === editing?.id ? (
                <input
                  className="view-tab-label terminal-tab-title"
                  style={{ width: editing.width }}
                  aria-label="Terminal tab title"
                  defaultValue={tab.title}
                  autoFocus
                  onFocus={(event) => {
                    composition.reset();
                    event.currentTarget.select();
                  }}
                  onCompositionStart={composition.start}
                  onCompositionEnd={composition.end}
                  onBlur={(event) => {
                    const title = event.currentTarget.value.trim();
                    if (title && title !== tab.title) {
                      setTabs((current) => current.map((item) =>
                        item.id === tab.id ? { ...item, title } : item,
                      ));
                    }
                    setEditing(null);
                  }}
                  onKeyDown={(event) => {
                    if (event.key !== "Enter" && event.key !== "Escape") return;
                    if (isImeEvent(event.nativeEvent) || composition.active()) return;
                    event.preventDefault();
                    const input = event.currentTarget;
                    if (event.key === "Escape") input.value = tab.title;
                    input.blur();
                    if (activeId) handles.current.get(activeId)?.focus();
                  }}
                />
              ) : (
                <button
                  type="button"
                  role="tab"
                  aria-selected={tab.id === activeId}
                  aria-keyshortcuts="F2"
                  className="view-tab-label"
                  title={`${tab.title} · Double-click to rename`}
                  onClick={() => {
                    setSelected(tab.id);
                    handles.current.get(tab.id)?.focus();
                  }}
                  onDoubleClick={(event) => beginRename(tab.id, event.currentTarget)}
                  onKeyDown={(event) => {
                    if (event.key !== "F2") return;
                    event.preventDefault();
                    beginRename(tab.id, event.currentTarget);
                  }}
                >
                  {tab.title}
                </button>
              )}
              <button
                type="button"
                className="view-tab-close"
                onClick={() => closeTab(tab.id)}
                aria-label={`Close ${tab.title}`}
                title="Close terminal"
              >
                <CloseRegular size={11} />
              </button>
            </div>
          ))}
        </div>
        <button
          type="button"
          className="ghost icon-button view-add"
          onClick={addTab}
          aria-label="New terminal"
          title="New terminal"
        >
          <AddRegular size={14} />
        </button>
        {onQuote && <QuoteToChatButton disabled={!activeId || !selections[activeId]} onClick={() => {
          const tab = tabs.find((tab) => tab.id === activeId);
          if (!tab) return;
          const text = handles.current.get(tab.id)?.getSelection();
          if (text) onQuote(quoteExcerpt(`${tab.title} output`, text));
        }} />}
      </header>

      {tabs.map((tab) => (
        <TerminalPane
          key={tab.id}
          workspace={workspace}
          active={active && tab.id === activeId}
          onSelectionChange={(selected) => setSelections((current) => ({ ...current, [tab.id]: selected }))}
          ref={(handle) => {
            if (handle) handles.current.set(tab.id, handle);
            else handles.current.delete(tab.id);
          }}
        />
      ))}
    </section>
  );
}
