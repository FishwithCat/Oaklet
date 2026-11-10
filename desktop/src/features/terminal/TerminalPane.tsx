import {
  useEffect,
  useImperativeHandle,
  useRef,
  type Ref,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { commandForTerminal } from "./terminal-command";

type TerminalOutput = { id: string; sequence: number; data: number[] };
type TerminalExit = { id: string; code: number | null };

export type TerminalPaneHandle = {
  focus: () => void;
  run: (command: string) => void;
};

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

export function newSessionId(): string {
  return (
    crypto.randomUUID?.() ??
    `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  );
}

/** One PTY-backed xterm instance. Kept mounted while its view is hidden. */
export function TerminalPane({
  active,
  ref,
}: {
  active: boolean;
  ref?: Ref<TerminalPaneHandle>;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const teardownRef = useRef<() => void>(() => {});
  const sessionIdRef = useRef<string | null>(null);
  const openedRef = useRef(false);
  const readyRef = useRef(false);
  const pendingRef = useRef<string[]>([]);

  function flushPending() {
    const sessionId = sessionIdRef.current;
    if (!sessionId) return;
    const pending = pendingRef.current;
    pendingRef.current = [];
    for (const data of pending) {
      void invoke("terminal_write", { id: sessionId, data });
    }
  }

  function run(command: string) {
    const data = commandForTerminal(command);
    if (!data) return;
    if (readyRef.current && sessionIdRef.current) {
      void invoke("terminal_write", { id: sessionIdRef.current, data });
    } else {
      pendingRef.current.push(data);
    }
  }

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
    const sessionId = newSessionId();
    sessionIdRef.current = sessionId;
    openedRef.current = false;
    readyRef.current = false;
    const unlisteners: UnlistenFn[] = [];
    let flushTimer: number | undefined;

    // Wait for startup output to settle so the command is not echoed before
    // the prompt. The shell is reading input once its output goes quiet.
    const flushWhenQuiet = () => {
      if (readyRef.current) return;
      window.clearTimeout(flushTimer);
      flushTimer = window.setTimeout(() => {
        if (disposed || readyRef.current) return;
        readyRef.current = true;
        flushPending();
      }, 50);
    };

    const start = async () => {
      openedRef.current = false;
      try {
        await invoke("terminal_open", {
          id: sessionId,
          cols: term.cols,
          rows: term.rows,
        });
        if (disposed) return;
        openedRef.current = true;
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
      if (openedRef.current) {
        void invoke("terminal_write", { id: sessionId, data });
      }
    });
    const stopResize = term.onResize(({ cols, rows }) => {
      if (openedRef.current) {
        void invoke("terminal_resize", { id: sessionId, cols, rows });
      }
    });
    const observer = new ResizeObserver(fitToHost);
    observer.observe(host);

    void (async () => {
      const offOutput = await listen<TerminalOutput>(
        "terminal://output",
        (event) => {
          if (event.payload.id !== sessionId) return;
          term.write(new Uint8Array(event.payload.data), () => {
            if (!disposed) {
              void invoke("terminal_ack", { id: sessionId, sequence: event.payload.sequence })
                .catch((error) => {
                  if (disposed || !openedRef.current) return;
                  openedRef.current = false;
                  readyRef.current = false;
                  term.write(`\r\n[Terminal closed: output acknowledgement failed: ${String(error)}]\r\n`);
                  void invoke("terminal_close", { id: sessionId });
                });
            }
          });
          flushWhenQuiet();
        },
      );
      const offExit = await listen<TerminalExit>("terminal://exit", (event) => {
        if (event.payload.id !== sessionId) return;
        openedRef.current = false;
        readyRef.current = false;
        sessionIdRef.current = null;
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
      window.clearTimeout(flushTimer);
      openedRef.current = false;
      readyRef.current = false;
      sessionIdRef.current = null;
      pendingRef.current = [];
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
      run,
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
