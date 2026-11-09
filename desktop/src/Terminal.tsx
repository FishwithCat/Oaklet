import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Ref,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  PlusIcon,
  TerminalWindowIcon,
  XIcon,
} from "@phosphor-icons/react";
import "@xterm/xterm/css/xterm.css";

type TerminalOutput = { id: string; data: number[] };
type TerminalExit = { id: string; code: number | null };

type TerminalHandle = {
  focus: () => void;
};

type Tab = { id: string; title: string };

function getTerminalTheme() {
  const style = getComputedStyle(document.documentElement);
  return {
    background: "rgba(0, 0, 0, 0)",
    foreground: style.getPropertyValue("--text").trim(),
    cursor: style.getPropertyValue("--accent").trim(),
    cursorAccent: style.getPropertyValue("--bg").trim(),
    selectionBackground: style.getPropertyValue("--terminal-selection").trim(),
    black: style.getPropertyValue("--terminal-black").trim(),
    red: style.getPropertyValue("--terminal-red").trim(),
    green: style.getPropertyValue("--terminal-green").trim(),
    yellow: style.getPropertyValue("--terminal-yellow").trim(),
    blue: style.getPropertyValue("--terminal-blue").trim(),
    magenta: style.getPropertyValue("--terminal-magenta").trim(),
    cyan: style.getPropertyValue("--terminal-cyan").trim(),
    white: style.getPropertyValue("--terminal-white").trim(),
    brightBlack: style.getPropertyValue("--terminal-bright-black").trim(),
    brightRed: style.getPropertyValue("--terminal-bright-red").trim(),
    brightGreen: style.getPropertyValue("--terminal-bright-green").trim(),
    brightYellow: style.getPropertyValue("--terminal-bright-yellow").trim(),
    brightBlue: style.getPropertyValue("--terminal-bright-blue").trim(),
    brightMagenta: style.getPropertyValue("--terminal-bright-magenta").trim(),
    brightCyan: style.getPropertyValue("--terminal-bright-cyan").trim(),
    brightWhite: style.getPropertyValue("--terminal-bright-white").trim(),
  };
}

function newSessionId(): string {
  return (
    crypto.randomUUID?.() ??
    `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  );
}

/** One PTY-backed xterm instance. Kept mounted while its view is hidden. */
function TerminalPane({
  active,
  ref,
}: {
  active: boolean;
  ref?: Ref<TerminalHandle>;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const teardownRef = useRef<() => void>(() => {});

  useEffect(() => {
    if (!active || termRef.current) return;
    const host = hostRef.current;
    if (!host) return;

    const term = new XTerm({
      allowTransparency: true,
      cursorBlink: true,
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Monaco, "Cascadia Mono", monospace',
      fontSize: 12,
      lineHeight: 1.45,
      scrollback: 5000,
      theme: getTerminalTheme(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    termRef.current = term;
    fitRef.current = fit;

    let disposed = false;
    let opened = false;
    const sessionId = newSessionId();
    const unlisteners: UnlistenFn[] = [];

    const start = async () => {
      opened = false;
      try {
        await invoke("terminal_open", {
          id: sessionId,
          cols: term.cols,
          rows: term.rows,
        });
        opened = !disposed;
      } catch (error) {
        if (!disposed) {
          term.write(`\r\n\x1b[31m${String(error)}\x1b[0m\r\n`);
        }
      }
    };

    const fitToHost = () => {
      if (
        host.clientWidth > 0 &&
        host.clientHeight > 0 &&
        termRef.current === term
      ) {
        fit.fit();
      }
    };

    const stopInput = term.onData((data) => {
      if (opened) void invoke("terminal_write", { id: sessionId, data });
    });
    const stopResize = term.onResize(({ cols, rows }) => {
      if (opened) void invoke("terminal_resize", { id: sessionId, cols, rows });
    });
    const observer = new ResizeObserver(fitToHost);
    observer.observe(host);

    void (async () => {
      const offOutput = await listen<TerminalOutput>(
        "terminal://output",
        (event) => {
          if (event.payload.id !== sessionId) return;
          term.write(new Uint8Array(event.payload.data));
        },
      );
      const offExit = await listen<TerminalExit>("terminal://exit", (event) => {
        if (event.payload.id !== sessionId) return;
        opened = false;
        const code = event.payload.code;
        const detail = code === null ? "" : ` with code ${code}`;
        term.write(`\r\n\x1b[2m[process exited${detail}]\x1b[0m\r\n`);
      });
      unlisteners.push(offOutput, offExit);
      if (disposed) {
        offOutput();
        offExit();
        return;
      }
      fitToHost();
      await start();
      if (!disposed) term.focus();
    })();

    teardownRef.current = () => {
      disposed = true;
      observer.disconnect();
      stopInput.dispose();
      stopResize.dispose();
      for (const unlisten of unlisteners) unlisten();
      void invoke("terminal_close", { id: sessionId });
      term.dispose();
      if (termRef.current === term) {
        termRef.current = null;
        fitRef.current = null;
      }
      teardownRef.current = () => {};
    };
  }, [active]);

  useEffect(() => () => teardownRef.current(), []);

  useEffect(() => {
    if (!active) return;
    const frame = requestAnimationFrame(() => {
      const host = hostRef.current;
      const term = termRef.current;
      if (host && term && host.clientWidth > 0 && host.clientHeight > 0) {
        term.options.theme = getTerminalTheme();
        fitRef.current?.fit();
      }
      term?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [active]);

  useImperativeHandle(
    ref,
    () => ({
      focus: () => termRef.current?.focus(),
    }),
    [],
  );

  return (
    <div
      className="terminal-screen"
      ref={hostRef}
      hidden={!active}
      role="tabpanel"
    />
  );
}

export function Terminal({
  active,
  onClose,
}: {
  active: boolean;
  onClose: () => void;
}) {
  const [tabs, setTabs] = useState<Tab[]>(() => [
    { id: newSessionId(), title: "Terminal 1" },
  ]);
  const [selected, setSelected] = useState<string | null>(null);
  const handles = useRef(new Map<string, TerminalHandle>());
  const count = useRef(1);

  const activeId = selected ?? tabs[0]?.id ?? null;

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
      <header className="terminal-head">
        <span className="terminal-icon" aria-hidden="true">
          <TerminalWindowIcon size={14} />
        </span>
        <div className="terminal-tabs" role="tablist" aria-label="Terminals">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`terminal-tab${tab.id === activeId ? " active" : ""}`}
            >
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === activeId}
                className="terminal-tab-label"
                onClick={() => {
                  setSelected(tab.id);
                  handles.current.get(tab.id)?.focus();
                }}
              >
                {tab.title}
              </button>
              <button
                type="button"
                className="terminal-tab-close"
                onClick={() => closeTab(tab.id)}
                aria-label={`Close ${tab.title}`}
                title="Close terminal"
              >
                <XIcon size={11} />
              </button>
            </div>
          ))}
        </div>
        <button
          type="button"
          className="ghost icon-button terminal-add"
          onClick={addTab}
          aria-label="New terminal"
          title="New terminal"
        >
          <PlusIcon size={14} />
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
