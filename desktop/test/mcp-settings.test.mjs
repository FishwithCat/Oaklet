import assert from "node:assert/strict";
import test from "node:test";
import {
  formatMcpConfig,
  parseMcpConfig,
  serverEnabled,
  serverTransport,
  setServerEnabled,
} from "../src/features/settings/mcp-settings.ts";

const standard = {
  mcpServers: {
    zhihu: {
      type: "streamableHttp",
      url: "https://developer.zhihu.com/api/mcp/v1",
      headers: { Authorization: "Bearer secret" },
    },
  },
};

test("parses the standard mcpServers document", () => {
  const result = parseMcpConfig(JSON.stringify(standard));
  assert.ok("servers" in result);
  assert.equal(result.servers.zhihu.type, "streamableHttp");
  assert.equal(result.servers.zhihu.url, "https://developer.zhihu.com/api/mcp/v1");
  assert.equal(result.servers.zhihu.headers.Authorization, "Bearer secret");
});

test("accepts a bare server map and reports invalid JSON", () => {
  const bare = parseMcpConfig('{"files":{"command":"npx"}}');
  assert.ok("servers" in bare);
  assert.equal(bare.servers.files.command, "npx");

  const broken = parseMcpConfig("{not json");
  assert.ok("error" in broken);
  assert.match(broken.error, /not valid JSON/);
});

test("formats the standard wrapper and round-trips", () => {
  const text = formatMcpConfig({
    files: { command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem"] },
  });
  assert.match(text, /"mcpServers"/);
  const parsed = parseMcpConfig(text);
  assert.ok("servers" in parsed);
  assert.deepEqual(parsed.servers.files.args, [
    "-y",
    "@modelcontextprotocol/server-filesystem",
  ]);
});

test("toggles enabled per server without dropping other fields", () => {
  const disabled = setServerEnabled(standard.mcpServers, "zhihu", false);
  assert.equal(disabled.zhihu.enabled, false);
  assert.equal(disabled.zhihu.url, standard.mcpServers.zhihu.url);
  assert.equal(serverEnabled(disabled.zhihu), false);

  const enabled = setServerEnabled(disabled, "zhihu", true);
  assert.equal(enabled.zhihu.enabled, undefined);
  assert.equal(serverEnabled(enabled.zhihu), true);
});

test("infers the transport for configs that omit type", () => {
  assert.equal(serverTransport({ url: "https://example.com/mcp" }), "streamableHttp");
  assert.equal(serverTransport({ command: "npx" }), "stdio");
  assert.equal(
    serverTransport({ command: "npx", url: "https://example.com/mcp" }),
    "stdio",
  );
  assert.equal(serverTransport({ type: "http", url: "https://example.com" }), "streamableHttp");
});
