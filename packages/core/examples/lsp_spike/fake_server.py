"""Deterministic stdio peer for the Rust spike tests; no third-party packages."""
import json
import sys
import time


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        key, value = line.decode().strip().split(": ", 1)
        if key.lower() == "content-length":
            length = int(value)
    assert length is not None
    return json.loads(sys.stdin.buffer.read(length))


pending = None
cancelled = False
while (message := read()) is not None:
    method = message.get("method")
    if method == "initialize":
        assert message["params"]["capabilities"]["textDocument"]["callHierarchy"] == {"dynamicRegistration": False}
        pending = message["id"]
        send({"jsonrpc": "2.0", "id": "config", "method": "workspace/configuration", "params": {"items": [{"section": "rust-analyzer"}]}})
        send({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 3, "message": "interleaved"}})
    elif message.get("id") == "config":
        assert message["result"] == [{"test": True}], message
        send({"jsonrpc": "2.0", "id": pending, "result": {"capabilities": {"definitionProvider": True}}})
    elif method == "textDocument/definition":
        time.sleep(0.15)
        send({"jsonrpc": "2.0", "id": message["id"], "result": []})
    elif method == "$/cancelRequest":
        cancelled = True
    elif method == "workspace/symbol":
        send({"jsonrpc": "2.0", "id": message["id"], "result": [{"name": "cancelled" if cancelled else "not-cancelled", "kind": 12,
            "location": {"uri": "file:///spike.rs", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}}]})
    elif method == "textDocument/documentSymbol":
        send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "unsupported"}})
    elif method == "textDocument/references":
        sys.stdout.buffer.write(b"Content-Length: 1\r\n\r\n!")
        sys.stdout.buffer.flush()
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
