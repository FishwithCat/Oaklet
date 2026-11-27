export function formatLineSpan(first: number, last: number): string {
  return first === last ? String(first) : `${first}-${last}`;
}

export function quoteExcerpt(source: string, text: string): string {
  const body = `${source}\n\n${text}`.replace(/\r\n?/g, "\n");
  let fenceLength = 3;
  for (const [run] of body.matchAll(/`+/g)) {
    fenceLength = Math.max(fenceLength, run.length + 1);
  }
  const fence = "`".repeat(fenceLength);
  return [fence, ...body.split("\n"), fence].map((line) => `> ${line}`).join("\n");
}
