import type { McpServers } from "./mcp-settings";

export type AppConfig = {
  baseUrl: string;
  apiKey: string;
  model: string;
  searchUrl: string;
  mcpServers: McpServers;
  workspace: string;
};
