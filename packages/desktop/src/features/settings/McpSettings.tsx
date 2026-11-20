import {
  serverEnabled,
  serverTransport,
  type McpConfigParse,
} from "./mcp-settings";

export function McpSettings({
  text,
  parsed,
  onChange,
  onToggleServer,
}: {
  text: string;
  parsed: McpConfigParse;
  onChange: (text: string) => void;
  onToggleServer: (name: string, enabled: boolean) => void;
}) {
  return (
    <>
      <p className="field-hint mcp-intro">
        Paste a standard <code>mcpServers</code> document. Each server can be
        enabled or disabled independently; a disabled server keeps its settings
        but exposes no tools.
      </p>

      <div className="field">
        <label htmlFor="mcp-json">Configuration JSON</label>
        <textarea
          id="mcp-json"
          className="mcp-json"
          spellCheck={false}
          value={text}
          onChange={(event) => onChange(event.target.value)}
        />
        <p className="field-hint">
          Use <code>type</code> <code>stdio</code> or{" "}
          <code>streamableHttp</code>. Set <code>"enabled": false</code> to
          pause a server. Commands run in the workspace; environment and header
          values are stored on this machine.
        </p>
      </div>

      {"error" in parsed ? (
        <p className="error-text mcp-json-error">{parsed.error}</p>
      ) : (
        <>
          <div className="mcp-heading">
            <h3>Servers</h3>
          </div>
          {Object.keys(parsed.servers).length === 0 ? (
            <p className="mcp-empty">No MCP servers in this config.</p>
          ) : (
            <div className="mcp-toggles">
              {Object.entries(parsed.servers).map(([name, server]) => (
                <label className="mcp-toggle" key={name}>
                  <input
                    type="checkbox"
                    checked={serverEnabled(server)}
                    onChange={(event) =>
                      onToggleServer(name, event.target.checked)
                    }
                  />
                  <span className="mcp-toggle-name">{name}</span>
                  <span className="mcp-toggle-type">
                    {serverTransport(server)}
                  </span>
                </label>
              ))}
            </div>
          )}
        </>
      )}
    </>
  );
}
