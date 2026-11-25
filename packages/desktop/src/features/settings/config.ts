import type { McpServers } from "./mcp-settings";
import type { Theme } from "./theme";

export const DEFAULT_SEARCH_URL = "https://api.deepseek.com/v1/tools/search";

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  theme: Theme;
  summaryModel: string;
  summaryContextWindow: number | null;
  contextWindow: number;
  searchUrl: string;
  searchApiKey: string;
  allowAllCommands: boolean;
  keepWorktreesOnArchive: boolean;
  mcpServers: McpServers;
  workspace: string;
};
