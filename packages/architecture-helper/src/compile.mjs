#!/usr/bin/env node
/**
 * Oaklet's pinned LikeC4 compiler.
 *
 * Protocol: one JSON request on stdin, one JSON response on stdout.
 *
 *   { "version": 1, "sources": { "architecture/model.c4": "..." } }
 *
 * A planning draft adds one virtual source and asks for its validated summary:
 *
 *   { "version": 1, "sources": { ... }, "diff": { "path": "...", "text": "..." } }
 *
 * The review view asks for one layouted compare diagram as well:
 *
 *   { "version": 1, "sources": { ... }, "diff": { ... },
 *     "compare": { "viewId": "containers" } }
 *
 * The helper only receives model sources. It never reads a workspace, imports
 * workspace configuration files, or executes workspace JavaScript. `fromSources`
 * builds a virtual workspace in memory and `layoutedModel()` runs Graphviz as
 * WebAssembly, so compilation leaves no temporary files behind.
 */
import { createCompareModels } from "@likec4/core/compute-view";

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
  return {
    sources,
    diff: validateDiff(request.diff, sources),
    compare: validateCompare(request.compare),
  };
}

function safePath(path) {
  return (
    typeof path === "string" &&
    path.length > 0 &&
    !path.startsWith("/") &&
    !path.includes("\\") &&
    !path.split("/").includes("..")
  );
}

function validateDiff(diff, sources) {
  if (diff === undefined) return null;
  if (!diff || typeof diff !== "object" || Array.isArray(diff)) {
    throw new Error("The architecture diff must be an object.");
  }
  if (!safePath(diff.path)) {
    throw new Error(`Unsafe architecture diff path: ${diff.path}`);
  }
  if (Object.hasOwn(sources, diff.path)) {
    throw new Error(`The architecture diff path collides with a source: ${diff.path}`);
  }
  if (typeof diff.text !== "string" || diff.text.trim().length === 0) {
    throw new Error("The architecture diff has no DSL.");
  }
  return { path: diff.path, text: diff.text };
}

function validateCompare(compare) {
  if (compare === undefined) return null;
  if (!compare || typeof compare !== "object" || Array.isArray(compare)) {
    throw new Error("The architecture compare request must be an object.");
  }
  if (typeof compare.viewId !== "string" || compare.viewId.trim().length === 0) {
    throw new Error("The architecture compare request has no view ID.");
  }
  return { viewId: compare.viewId };
}

function documentFor(likec4, path) {
  return [...likec4.langium.likec4.shared.workspace.LangiumDocuments.all].find(
    (document) =>
      document.uri.path === `${VIRTUAL_ROOT}/${path}` ||
      document.uri.path.endsWith(`/${path}`),
  );
}

function topLevelDiffs(document) {
  return document?.parseResult?.value?.diffs ?? [];
}

function richText(value) {
  if (typeof value === "string") return value;
  if (value && typeof value.txt === "string") return value.txt;
  return null;
}

function diffSummary(summary, changes) {
  return {
    id: summary.id,
    title: summary.title,
    description: richText(summary.description),
    added: summary.added,
    removed: summary.removed,
    modified: summary.modified,
    views: summary.views ?? [],
    ...(changes ? { changes } : {}),
  };
}

function modelRef(value) {
  if (typeof value === "string") return value;
  if (value && typeof value.model === "string") return value.model;
  return null;
}

function elementTitle(element, fallback) {
  return richText(element?.title) ?? fallback;
}

function elementChange(id, element) {
  return {
    id,
    title: elementTitle(element, id),
    kind: typeof element?.kind === "string" ? element.kind : null,
  };
}

function relationChange(relation, elements) {
  const source = modelRef(relation?.source) ?? "";
  const target = modelRef(relation?.target) ?? "";
  return {
    id: typeof relation?.id === "string" ? relation.id : "",
    source,
    sourceTitle: elementTitle(elements?.[source], source),
    target,
    targetTitle: elementTitle(elements?.[target], target),
    label: richText(relation?.title) ?? "",
  };
}

const MODIFIED_FIELDS = [
  "title",
  "description",
  "technology",
  "tags",
  "links",
  "metadata",
];

/** Names for the review checklist, built from the parsed diff and both models. */
function diffChanges(models) {
  if (!models) return null;
  const { diff, before, after } = models;
  const beforeElements = before?.$data?.elements ?? {};
  const afterElements = after?.$data?.elements ?? {};
  const beforeRelations = before?.$data?.relations ?? {};
  return {
    added: {
      elements: Object.values(diff.add.elements).map((element) =>
        elementChange(element.id, element),
      ),
      relations: Object.values(diff.add.relations).map((relation) =>
        relationChange(relation, afterElements),
      ),
    },
    removed: {
      elements: diff.remove.elements.map((id) =>
        elementChange(id, beforeElements[id]),
      ),
      relations: diff.remove.relations.map((id) =>
        relationChange(beforeRelations[id], beforeElements),
      ),
    },
    modified: {
      elements: Object.entries(diff.modify.elements).map(([id, overlay]) => {
        const element = afterElements[id] ?? beforeElements[id];
        return {
          ...elementChange(id, element),
          fields: Object.keys(overlay ?? {}).filter((field) =>
            MODIFIED_FIELDS.includes(field),
          ),
        };
      }),
    },
  };
}

async function changesForDraft(likec4, id) {
  try {
    const parsed = await likec4.modelBuilder.parseModel();
    return parsed ? diffChanges(createCompareModels(parsed.$data, id)) : null;
  } catch {
    // The checklist is optional; validation and the diff summary still stand.
    return null;
  }
}

async function validateDraft(likec4, diff, diagnostics) {
  const draftDocument = documentFor(likec4, diff.path);
  const draftDiffs = topLevelDiffs(draftDocument);
  if (draftDiffs.length !== 1) {
    return {
      ok: false,
      error: `A plan draft must contain exactly one top-level diff; found ${draftDiffs.length}.`,
      diagnostics,
      draftError: true,
    };
  }

  const id = draftDiffs[0].name;
  const existing = [];
  for (const document of likec4.langium.likec4.shared.workspace.LangiumDocuments.all) {
    if (document === draftDocument) continue;
    for (const other of topLevelDiffs(document)) existing.push(other.name);
  }
  if (existing.includes(id)) {
    return {
      ok: false,
      error: `A diff with ID "${id}" already exists in this model.`,
      diagnostics,
      draftError: true,
    };
  }

  const summaries = await likec4.modelDiffs.list();
  const summary = summaries.find((candidate) => candidate.id === id);
  if (!summary) {
    return {
      ok: false,
      error: `LikeC4 did not report a diff with ID "${id}".`,
      diagnostics,
      draftError: true,
    };
  }

  const check = likec4.modelDiffs.checkBeforeApply(id);
  const warnings = likec4.getWarnings().map(diagnosticFrom);
  if (check.errors.length > 0) {
    return {
      ok: false,
      error: "The LikeC4 diff has errors.",
      diagnostics: [
        ...diagnostics,
        ...check.errors.map((message) => ({
          message,
          file: diff.path,
          line: null,
          column: null,
        })),
      ],
      warnings,
      overlaps: check.overlaps,
      draftError: true,
    };
  }

  return {
    ok: true,
    diff: diffSummary(summary, await changesForDraft(likec4, id)),
    warnings,
    overlaps: check.overlaps,
  };
}

async function compareDraft(likec4, diff, viewId) {
  const validation = await validateDraft(likec4, diff, []);
  if (!validation.ok) return validation;
  const view = (validation.diff.views ?? []).find(
    (candidate) => candidate.id === viewId,
  );
  if (!view) {
    return {
      ok: false,
      error: `The diff does not change view "${viewId}".`,
      diagnostics: [],
    };
  }
  const compareView = await likec4.modelDiffs.layoutCompareView({
    viewId,
    diffId: validation.diff.id,
  });
  if (!compareView) {
    return {
      ok: false,
      error: `LikeC4 could not lay out the compare view "${viewId}".`,
      diagnostics: [],
    };
  }
  const layouted = await likec4.layoutedModel();
  const model = JSON.parse(JSON.stringify(layouted.$data));
  // The desktop only needs the model metadata and this one compare view; the
  // compare view keeps its own layout for all three review states.
  model.views = { [compareView.id]: compareView };
  return {
    ok: true,
    diff: validation.diff,
    view: compareView,
    model,
  };
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
    const { sources, diff, compare } = validateRequest(await readRequest());
    const allSources = diff ? { ...sources, [diff.path]: diff.text } : sources;
    const { fromSources } = await import("@likec4/language-services/node");
    likec4 = await fromSources(allSources, {
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
      const draftError =
        diff !== null &&
        diagnostics.every((diagnostic) => diagnostic.file === diff.path);
      respond({
        ok: false,
        error: diff
          ? draftError
            ? "The LikeC4 diff has errors."
            : "The LikeC4 model has errors."
          : "The LikeC4 model has errors.",
        diagnostics,
        views: [],
        draftError,
      });
      return;
    }

    if (compare) {
      if (!diff) {
        respond({
          ok: false,
          error: "The architecture compare request needs a diff.",
          diagnostics: [],
          views: [],
        });
        return;
      }
      respond(await compareDraft(likec4, diff, compare.viewId));
      return;
    }

    if (diff) {
      respond(await validateDraft(likec4, diff, diagnostics));
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
