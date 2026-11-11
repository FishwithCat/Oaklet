import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { SessionView } from "./session-model.ts";

export type DeltaEvent = { sessionId: string; runId: string; text: string };
export type ReasoningEvent = { sessionId: string; runId: string; text: string };
export type ToolStartEvent = {
  sessionId: string;
  runId: string;
  id: string;
  name: string;
  summary: string;
};
export type ToolResultEvent = {
  sessionId: string;
  runId: string;
  id: string;
  ok: boolean;
  output: string;
};
export type ApprovalEvent = {
  sessionId: string;
  runId: string;
  id: string;
  requestId: string;
  tool: string;
  prompt: string;
  detail: string;
};
export type ApprovalResolvedEvent = {
  sessionId: string;
  runId: string;
  id: string;
  allow: boolean;
};
export type DoneEvent = {
  sessionId: string;
  runId: string;
  session: SessionView | null;
};
export type ContextEvent = {
  sessionId: string;
  runId: string;
  used: number;
  window: number;
};
export type ErrorEvent = {
  sessionId: string;
  runId: string;
  message: string;
  session: SessionView | null;
};
export type StoppedEvent = {
  sessionId: string;
  runId: string;
  session: SessionView | null;
};

export type AgentEventHandlers = {
  onDelta: (event: DeltaEvent) => void;
  onReasoning: (event: ReasoningEvent) => void;
  onToolStart: (event: ToolStartEvent) => void;
  onToolResult: (event: ToolResultEvent) => void;
  onApproval: (event: ApprovalEvent) => void;
  onApprovalResolved: (event: ApprovalResolvedEvent) => void;
  onDone: (event: DoneEvent) => void;
  onContext: (event: ContextEvent) => void;
  onError: (event: ErrorEvent) => void;
  onStopped: (event: StoppedEvent) => void;
};

export type RegisterListener = <T>(
  event: string,
  handler: (payload: T) => void,
) => Promise<UnlistenFn>;

const tauriRegister: RegisterListener = <T,>(
  event: string,
  handler: (payload: T) => void,
) => listen<T>(event, (payload) => handler(payload.payload));

/**
 * Register the agent event listeners and return one release function. A failed
 * registration releases the listeners that did succeed before it.
 */
export async function subscribeAgentEvents(
  handlers: AgentEventHandlers,
  register: RegisterListener = tauriRegister,
): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];

  async function add<T>(
    event: string,
    handler: (payload: T) => void,
  ): Promise<void> {
    unlisteners.push(await register(event, handler));
  }

  try {
    await add<DeltaEvent>("agent:delta", (payload) => handlers.onDelta(payload));
    await add<ReasoningEvent>("agent:reasoning", (payload) =>
      handlers.onReasoning(payload),
    );
    await add<ToolStartEvent>("agent:tool_start", (payload) =>
      handlers.onToolStart(payload),
    );
    await add<ToolResultEvent>("agent:tool_result", (payload) =>
      handlers.onToolResult(payload),
    );
    await add<ApprovalEvent>("agent:approval", (payload) =>
      handlers.onApproval(payload),
    );
    await add<ApprovalResolvedEvent>("agent:approval_resolved", (payload) =>
      handlers.onApprovalResolved(payload),
    );
    await add<DoneEvent>("agent:done", (payload) => handlers.onDone(payload));
    await add<ContextEvent>("agent:context", (payload) =>
      handlers.onContext(payload),
    );
    await add<ErrorEvent>("agent:error", (payload) => handlers.onError(payload));
    await add<StoppedEvent>("agent:stopped", (payload) =>
      handlers.onStopped(payload),
    );
  } catch (error) {
    for (const unlisten of unlisteners) unlisten();
    throw error;
  }

  return () => {
    for (const unlisten of unlisteners) unlisten();
  };
}
