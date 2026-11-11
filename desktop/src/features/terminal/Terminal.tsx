import { useImperativeHandle, useRef, useState, type Ref } from "react";
import { AddRegular, CloseRegular, TerminalBoxRegular } from "@mingcute/react/core-regular";
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
  onClose,
}: {
  ref?: Ref<TerminalHandle>;
  active: boolean;
  onClose: () => void;
}) {
  const [tabs, setTabs] = useState<Tab[]>(() => [
    { id: newSessionId(), title: "Terminal 1" },
  ]);
  const [selected, setSelected] = useState<string | null>(null);
  const handles = useRef(new Map<string, TerminalPaneHandle>());
  const count = useRef(1);

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

  function closeTab(id: string) {
    const index = tabs.findIndex((tab) => tab.id === id);
    if (index < 0) return;

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
      <header className="view-head">
        <span className="view-icon" aria-hidden="true">
          <TerminalBoxRegular size={14} />
        </span>
        <div className="view-tabs" role="tablist" aria-label="Terminals">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`view-tab${tab.id === activeId ? " active" : ""}`}
            >
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === activeId}
                className="view-tab-label"
                onClick={() => {
                  setSelected(tab.id);
                  handles.current.get(tab.id)?.focus();
                }}
              >
                {tab.title}
              </button>
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
      </header>

      {tabs.map((tab) => (
        <TerminalPane
          key={tab.id}
          active={active && tab.id === activeId}
          ref={(handle) => {
            if (handle) handles.current.set(tab.id, handle);
            else handles.current.delete(tab.id);
          }}
        />
      ))}
    </section>
  );
}
