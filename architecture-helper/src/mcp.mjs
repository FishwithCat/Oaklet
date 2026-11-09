import { PassThrough } from "node:stream";
import { initLikeC4MCP } from "@likec4/mcp";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { ReadBuffer, serializeMessage } from "@modelcontextprotocol/sdk/shared/stdio.js";

// Keep the official stdio server in this process so cancellation kills all work.
// shortcut: reload per call; keep a session if workspace startup becomes too slow.
async function query(workspace, request) {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const buffer = new ReadBuffer();
  const transport = {
    async start() {
      stdout.on("data", (chunk) => {
        try {
          buffer.append(chunk);
          let message;
          while ((message = buffer.readMessage()) !== null) {
            transport.onmessage?.(message);
          }
        } catch (error) {
          transport.onerror?.(error);
        }
      });
    },
    async send(message) { stdin.write(serializeMessage(message)); },
    async close() { stdin.end(); stdout.destroy(); transport.onclose?.(); },
  };
  const { server, likec4 } = await initLikeC4MCP({
    workspacePath: workspace,
    mcp: "stdio",
    stdio: { stdin, stdout },
    watch: false,
    configureLogger: false,
  });
  const client = new Client({ name: "oaklet", version: "0.1.0" });
  try {
    await server.start();
    await client.connect(transport);
    if (request.tool) {
      return await client.callTool({ name: request.tool, arguments: request.arguments ?? {} });
    }
    const tools = [];
    let cursor;
    do {
      const page = await client.listTools({ cursor });
      tools.push(...page.tools.map(({ name, description, inputSchema }) => ({ name, description, inputSchema })));
      cursor = page.nextCursor;
    } while (cursor);
    return { tools };
  } finally {
    try {
      await client.close();
    } finally {
      try { await server.stop(); } finally { await likec4.dispose(); }
    }
  }
}

let input = "";
for await (const chunk of process.stdin) input += chunk;
try {
  const result = await query(process.argv[2], JSON.parse(input));
  process.stdout.write(JSON.stringify(result));
} catch (error) {
  process.stderr.write(String(error));
  process.exitCode = 1;
}
