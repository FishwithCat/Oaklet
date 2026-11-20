export type McpTransport = "stdio" | "streamableHttp";

export type McpServerConfig = {
  type?: McpTransport | "http" | "streamable_http";
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  enabled?: boolean;
  [key: string]: unknown;
};

export type McpServers = Record<string, McpServerConfig>;

export type McpConfigParse =
  | { servers: McpServers }
  | { error: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Pretty-print the standard `{ "mcpServers": { ... } }` document. */
export function formatMcpConfig(servers: McpServers): string {
  return JSON.stringify({ mcpServers: servers }, null, 2);
}

/**
 * Parse the standard document, accepting a bare server map as a convenience.
 * Only JSON shape is checked here; the Rust side validates commands and URLs.
 */
export function parseMcpConfig(text: string): McpConfigParse {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    return {
      error: `MCP config is not valid JSON: ${
        error instanceof Error ? error.message : String(error)
      }`,
    };
  }
  if (!isRecord(parsed)) {
    return { error: "MCP config must be a JSON object." };
  }

  const raw = isRecord(parsed.mcpServers) ? parsed.mcpServers : parsed;
  const servers: McpServers = {};
  for (const [name, value] of Object.entries(raw)) {
    if (!isRecord(value)) {
      return { error: `MCP server "${name}" must be a JSON object.` };
    }
    servers[name] = value as McpServerConfig;
  }
  return { servers };
}

export function serverEnabled(server: McpServerConfig): boolean {
  return server.enabled !== false;
}

export function serverTransport(server: McpServerConfig): string {
  if (typeof server.type === "string" && server.type.trim()) {
    return server.type === "http" || server.type === "streamable_http"
      ? "streamableHttp"
      : server.type;
  }
  if (typeof server.command === "string" && server.command.trim()) {
    return "stdio";
  }
  if (typeof server.url === "string" && server.url.trim()) {
    return "streamableHttp";
  }
  return "stdio";
}

export function setServerEnabled(
  servers: McpServers,
  name: string,
  enabled: boolean,
): McpServers {
  const server = { ...servers[name] };
  if (enabled) {
    delete server.enabled;
  } else {
    server.enabled = false;
  }
  return { ...servers, [name]: server };
}
