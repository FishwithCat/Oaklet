import type { McpServers } from "./mcp-settings";

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  summaryModel: string;
  contextWindow: number;
  searchUrl: string;
  mcpServers: McpServers;
  workspace: string;
};
