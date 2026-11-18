import type { McpServers } from "./mcp-settings";
import type { Theme } from "./theme";

export const DEFAULT_SEARCH_URL = "https://api.deepseek.com/v1/tools/search";

export type CodeIndexConfig = {
  servers: Partial<
    Record<
      "rust" | "typescript",
      {
        enabled?: boolean;
        command?: string | null;
        args?: string[];
      }
    >
  >;
};

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
  mcpServers: McpServers;
  codeIndex: CodeIndexConfig;
  workspace: string;
};
