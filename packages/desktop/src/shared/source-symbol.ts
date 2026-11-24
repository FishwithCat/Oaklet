export type SourceSymbol = {
  name: string;
  kind: string;
  container: string;
  startLine: number;
  endLine: number;
};

export type Outline = { symbols: SourceSymbol[]; hasErrors: boolean; truncated: boolean };

export function supportsOutline(path: string): boolean {
  return /\.(rs|[cm]?[jt]s|[jt]sx|py|java)$/i.test(path);
}

export function qualifiedSymbol(symbol: SourceSymbol): string {
  return symbol.container ? `${symbol.container} / ${symbol.name}` : symbol.name;
}
