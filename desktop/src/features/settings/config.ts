import type { McpServers } from "./mcp-settings";

export type SearchProvider = "deepseek" | "exa";

export const SEARCH_PROVIDERS: {
  id: SearchProvider;
  label: string;
  endpoint: string;
}[] = [
  {
    id: "deepseek",
    label: "DeepSeek",
    endpoint: "https://api.deepseek.com/v1/tools/search",
  },
  { id: "exa", label: "Exa", endpoint: "https://api.exa.ai/search" },
];

export function defaultSearchUrl(provider: SearchProvider): string {
  return SEARCH_PROVIDERS.find((item) => item.id === provider)?.endpoint ?? "";
}

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  summaryModel: string;
  contextWindow: number;
  searchProvider: SearchProvider;
  searchUrl: string;
  searchApiKey: string;
  allowAllCommands: boolean;
  mcpServers: McpServers;
  workspace: string;
};
