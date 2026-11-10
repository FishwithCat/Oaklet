import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type DeltaEvent = { runId: string; text: string };
export type ReasoningEvent = { runId: string; text: string };
export type ToolStartEvent = {
  runId: string;
  id: string;
  name: string;
  summary: string;
};
export type ToolResultEvent = {
  runId: string;
  id: string;
  ok: boolean;
  output: string;
};
export type ApprovalEvent = {
  runId: string;
  id: string;
  requestId: string;
  tool: string;
  prompt: string;
  detail: string;
};
export type DoneEvent = { runId: string; messages: unknown[] };
export type ErrorEvent = { runId: string; message: string };
export type RunIdEvent = { runId: string };

export type AgentEventHandlers = {
  onDelta: (event: DeltaEvent) => void;
  onReasoning: (event: ReasoningEvent) => void;
  onToolStart: (event: ToolStartEvent) => void;
  onToolResult: (event: ToolResultEvent) => void;
  onApproval: (event: ApprovalEvent) => void;
  onDone: (event: DoneEvent) => void;
  onError: (event: ErrorEvent) => void;
  onStopped: (event: RunIdEvent) => void;
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
    await add<DoneEvent>("agent:done", (payload) => handlers.onDone(payload));
    await add<ErrorEvent>("agent:error", (payload) => handlers.onError(payload));
    await add<RunIdEvent>("agent:stopped", (payload) =>
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
