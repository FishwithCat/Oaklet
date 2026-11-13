import type { McpServers } from "./mcp-settings";

export const DEFAULT_SEARCH_URL = "https://api.deepseek.com/v1/tools/search";

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  summaryModel: string;
  contextWindow: number;
  searchUrl: string;
  searchApiKey: string;
  allowAllCommands: boolean;
  mcpServers: McpServers;
  workspace: string;
};
