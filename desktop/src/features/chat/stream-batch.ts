type Delta = { kind: "assistant" | "reasoning"; runId: string; text: string };

export function createStreamBatch(consume: (deltas: Delta[]) => void) {
  let pending: Delta[] = [];
  let frame: number | undefined;

  function clear() {
    if (frame !== undefined) cancelAnimationFrame(frame);
    frame = undefined;
    pending = [];
  }

  function flush() {
    const deltas = pending;
    clear();
    if (deltas.length) consume(deltas);
  }

  return {
    push(delta: Delta) {
      const last = pending[pending.length - 1];
      if (last?.kind === delta.kind && last.runId === delta.runId) {
        last.text += delta.text;
      } else {
        pending.push({ ...delta });
      }
      if (frame === undefined) frame = requestAnimationFrame(flush);
    },
    flush,
    clear,
  };
}
