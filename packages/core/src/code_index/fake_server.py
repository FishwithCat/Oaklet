"""Saved-workspace lifecycle test peer, Python stdlib only."""
import json
import os
from pathlib import Path
import sys
import subprocess

if "--version" in sys.argv:
    print("oaklet-fake-lsp 1")
    sys.exit(0)

root = Path.cwd()
log = root / "events.jsonl"


def record(event, **extra):
    with log.open("a") as stream:
        stream.write(json.dumps({"event": event, "pid": os.getpid(), **extra}) + "\n")


def send(message):
    body = json.dumps({"jsonrpc": "2.0", **message}).encode()
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
    return json.loads(sys.stdin.buffer.read(length))


record("spawn")
if (root / "spawn-child").exists():
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
    record("descendant", pid=child.pid)
documents = {}
options = json.loads((root / "sync-options.json").read_text()) if (root / "sync-options.json").exists() else {}
encoding = options.get("encoding", "utf-16")
sync_kind = options.get("syncKind", 2)
initializing = None
waiting = {}
late = {}
while (message := read()) is not None:
    method = message.get("method")
    params = message.get("params", {})
    if method == "initialize":
        assert params["rootUri"] == root.as_uri() + "/", params
        assert params["capabilities"]["general"]["positionEncodings"] == ["utf-16", "utf-8", "utf-32"]
        record("initialize", options=params["initializationOptions"])
        initializing = message["id"]
        if (root / "hang-init").exists():
            continue
        send({"id": "config", "method": "workspace/configuration", "params": {"items": [{"section": "rust-analyzer"}]}})
    elif message.get("id") == "config":
        settings = message["result"][0]
        assert settings["cargo"]["buildScripts"]["enable"] is False
        assert settings["procMacro"]["enable"] is False
        send({"id": initializing, "result": {"capabilities": {"documentSymbolProvider": True, "definitionProvider": True, "referencesProvider": True, "callHierarchyProvider": False, "textDocumentSync": {"openClose": True, "change": sync_kind, "save": {"includeText": True}}, "positionEncoding": encoding}}})
    elif method == "initialized":
        record("ready")
        send({"id": "watch-registration", "method": "client/registerCapability", "params": {"registrations": [{"id": "sources", "method": "workspace/didChangeWatchedFiles", "registerOptions": {"watchers": [{"globPattern": "**/*.rs"}, *([{"globPattern": "**/*.oaklet-data"}] if options.get("watchData") else [])]}}]}})
    elif message.get("id") == "watch-registration":
        assert message.get("result") is None and "error" not in message, message
        record("watch-registered")
    elif method == "textDocument/didOpen":
        document = params["textDocument"]
        assert document["uri"] not in documents
        documents[document["uri"]] = document
        record("open", document=document)
    elif method == "textDocument/didChange":
        document = params["textDocument"]
        previous = documents[document["uri"]]
        assert document["version"] > previous["version"]
        change = params["contentChanges"][0]
        if sync_kind == 2:
            assert change["range"]["start"] == {"line": 0, "character": 0}
            lines = previous["text"].split("\n")
            tail = lines[-1].removesuffix("\r")
            width = len(tail.encode("utf-8")) if encoding == "utf-8" else len(tail) if encoding == "utf-32" else len(tail.encode("utf-16-le")) // 2
            assert change["range"]["end"] == {"line": len(lines) - 1, "character": width}
        else:
            assert "range" not in change
        documents[document["uri"]] = {**document, "text": change["text"]}
        record("change", document=documents[document["uri"]])
    elif method == "textDocument/didSave":
        document = documents[params["textDocument"]["uri"]]
        assert params["text"] == document["text"]
        record("save", uri=document["uri"])
    elif method == "textDocument/didClose":
        document = params["textDocument"]
        documents.pop(document["uri"], None)
        record("close", uri=document["uri"])
    elif method == "workspace/didChangeWatchedFiles":
        record("files", changes=params["changes"])
    elif method == "textDocument/definition":
        if params["position"]["line"] == 99:
            record("crash")
            os._exit(7)
        request_id = message["id"]
        record("query", id=request_id)
        send({"method": "$/progress", "params": {"token": "scan", "value": {"kind": "begin", "title": "Analyzing saved sources"}}})
        late[request_id] = params["textDocument"]["uri"]
        send({"id": f"edit-{request_id}", "method": "workspace/applyEdit", "params": {"edit": {"changes": {}}}})
    elif isinstance(message.get("id"), str) and message["id"].startswith("edit-"):
        assert message["result"]["applied"] is False
        record("edit-rejected")
    elif method == "$/cancelRequest":
        record("cancel", id=params["id"])
    elif method == "textDocument/documentSymbol":
        record("symbols", uri=params["textDocument"]["uri"])
        if (root / "hang-symbols").exists():
            continue
        if (root / "method-missing").exists():
            send({"id": message["id"], "error": {"code": -32601, "message": "not implemented"}})
            continue
        # A late response to the cancelled request must never satisfy this query.
        for request_id in late:
            send({"id": request_id, "result": []})
        late.clear()
        send({"method": "$/progress", "params": {"token": "scan", "value": {"kind": "end"}}})
        request_id = message["id"]
        waiting[f"progress-{request_id}"] = request_id
        send({"id": f"progress-{request_id}", "method": "window/workDoneProgress/create", "params": {"token": request_id}})
    elif isinstance(message.get("id"), str) and message["id"].startswith("progress-"):
        request_id = waiting.pop(message["id"])
        if options.get("unicodeResult"):
            send({"id": request_id, "result": [{"name": "alive", "kind": 12, "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}}, "selectionRange": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}}}]})
            continue
        send({"id": request_id, "result": [{"name": "alive", "kind": 12, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}}, "selectionRange": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}}}]})
    elif method == "textDocument/references":
        sys.stdout.buffer.write(b"Content-Length: 999999999\r\n\r\n")
        sys.stdout.buffer.flush()
    elif method == "shutdown":
        record("shutdown")
        if (root / "hang-shutdown").exists():
            continue
        send({"id": message["id"], "result": None})
    elif method == "exit":
        if (root / "hang-shutdown").exists():
            continue
        record("exit")
        break
