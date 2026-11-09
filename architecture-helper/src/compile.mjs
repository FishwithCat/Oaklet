#!/usr/bin/env node
/**
 * Oaklet's pinned LikeC4 compiler.
 *
 * Protocol: one JSON request on stdin, one JSON response on stdout.
 *
 *   { "version": 1, "sources": { "architecture/model.c4": "..." } }
 *
 * The helper only receives model sources. It never reads a workspace, imports
 * workspace configuration files, or executes workspace JavaScript. `fromSources`
 * builds a virtual workspace in memory and `layoutedModel()` runs Graphviz as
 * WebAssembly, so compilation leaves no temporary files behind.
 */
const HELPER_VERSION = "0.1.0";
const MAX_INPUT_BYTES = 8 * 1024 * 1024;
const MAX_OUTPUT_BYTES = 16 * 1024 * 1024;
const MAX_DIAGNOSTICS = 50;
const MAX_DIAGNOSTIC_CHARS = 2_000;
const VIRTUAL_ROOT = "/workspace";

if (process.argv.includes("--version")) {
  process.stdout.write(`${HELPER_VERSION}\n`);
  process.exit(0);
}

function diagnosticFrom(error) {
  const message = String(error?.message ?? error ?? "LikeC4 reported an error.");
  const source = String(error?.sourceFsPath ?? "");
  const file = source.startsWith(`${VIRTUAL_ROOT}/`)
    ? source.slice(VIRTUAL_ROOT.length + 1)
    : source || null;
  const line = Number.isInteger(error?.line) ? error.line : null;
  const column = Number.isInteger(error?.range?.start?.character)
    ? error.range.start.character + 1
    : null;
  return {
    message: message.slice(0, MAX_DIAGNOSTIC_CHARS),
    file,
    line,
    column,
  };
}

function viewsFrom(model) {
  const data = model.$data;
  return Object.entries(data.views ?? {}).map(([id, view]) => ({
    id,
    title: typeof view?.title === "string" && view.title ? view.title : id,
    kind: typeof view?._type === "string" ? view._type : "element",
  }));
}

async function readRequest() {
  const chunks = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > MAX_INPUT_BYTES) {
      throw new Error("The architecture request is too large.");
    }
    chunks.push(chunk);
  }
  if (chunks.length === 0) {
    throw new Error("The architecture request is empty.");
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function validateRequest(request) {
  if (!request || request.version !== 1) {
    throw new Error("Unsupported architecture request version.");
  }
  if (
    !request.sources ||
    typeof request.sources !== "object" ||
    Array.isArray(request.sources)
  ) {
    throw new Error("The architecture request has no sources.");
  }
  const sources = {};
  for (const [path, content] of Object.entries(request.sources)) {
    if (typeof path !== "string" || typeof content !== "string") {
      throw new Error("Architecture sources must map paths to text.");
    }
    if (
      path.startsWith("/") ||
      path.includes("\\") ||
      path.split("/").includes("..")
    ) {
      throw new Error(`Unsafe architecture source path: ${path}`);
    }
    sources[path] = content;
  }
  if (Object.keys(sources).length === 0) {
    throw new Error("The architecture request has no sources.");
  }
  return sources;
}

function respond(payload) {
  if (responded) return;
  responded = true;
  const text = JSON.stringify(payload);
  if (Buffer.byteLength(text) > MAX_OUTPUT_BYTES) {
    process.stdout.write(
      JSON.stringify({
        ok: false,
        error:
          "The layouted architecture model is too large to display. Reduce the number of elements or views.",
        diagnostics: [],
        views: [],
      }),
    );
    return;
  }
  process.stdout.write(text);
}

let responded = false;

async function main() {
  let likec4;
  try {
    const sources = validateRequest(await readRequest());
    const { fromSources } = await import("@likec4/language-services/node");
    likec4 = await fromSources(sources, {
      graphviz: "wasm",
      printErrors: false,
      throwIfInvalid: false,
      configureLogger: false,
    });

    const diagnostics = likec4
      .getErrors()
      .slice(0, MAX_DIAGNOSTICS)
      .map(diagnosticFrom);
    if (diagnostics.length > 0) {
      respond({
        ok: false,
        error: "The LikeC4 model has errors.",
        diagnostics,
        views: [],
      });
      return;
    }

    const layouted = await likec4.layoutedModel();
    const model = JSON.parse(JSON.stringify(layouted.$data));
    if (model?._stage !== "layouted") {
      respond({
        ok: false,
        error: "LikeC4 did not produce a layouted model.",
        diagnostics: [],
        views: [],
      });
      return;
    }

    respond({
      ok: true,
      projectId: layouted.projectId,
      model,
      views: viewsFrom(layouted),
      diagnostics: [],
    });
  } catch (error) {
    respond({
      ok: false,
      error: String(error?.message ?? error),
      diagnostics: [],
      views: [],
    });
  } finally {
    if (likec4) {
      await likec4.dispose().catch(() => {});
    }
  }
}

// Keep stdin open only until it has been read. The unhandled rejection guard
// makes sure a catastrophic failure still produces a parseable response.
process.on("unhandledRejection", (error) => {
  respond({
    ok: false,
    error: String(error?.message ?? error),
    diagnostics: [],
    views: [],
  });
  process.exitCode = 1;
});

await main();
