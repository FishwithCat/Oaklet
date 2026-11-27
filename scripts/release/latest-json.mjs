#!/usr/bin/env node
// Build and merge Tauri updater manifests for the platform release workflows.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function assetUrl({ repo, version, artifact }) {
  const filename = basename(artifact);
  return `https://github.com/${repo}/releases/download/v${version}/${encodeURIComponent(filename)}`;
}

export function signatureFor(path) {
  const signature = readFileSync(path, "utf8").trim();
  if (!signature) throw new Error(`Signature file is empty: ${path}`);
  return signature;
}

export function makeFragment({
  version,
  platform,
  artifact,
  signature,
  repo,
  notes = "",
  pubDate = new Date().toISOString(),
}) {
  if (!version || !platform || !artifact) {
    throw new Error("version, platform, and artifact are required");
  }
  return {
    version,
    notes,
    pub_date: pubDate,
    platforms: {
      [platform]: {
        signature: signatureFor(signature),
        url: assetUrl({ repo, version, artifact }),
      },
    },
  };
}

export function mergeManifests(base, overlays) {
  const sources = [base, ...overlays].filter(Boolean);
  if (sources.length === 0) throw new Error("No manifests to merge");
  const version = sources[0].version;
  if (!version) throw new Error("A manifest has no version");
  let notes = "";
  let pubDate = sources[0].pub_date;
  const platforms = {};
  for (const source of sources) {
    if (source.version !== version) {
      throw new Error(
        `Refusing to mix versions ${version} and ${source.version} in one manifest`,
      );
    }
    if (source.notes) notes = source.notes;
    if (source.pub_date) pubDate = source.pub_date;
    for (const [target, release] of Object.entries(source.platforms ?? {})) {
      if (!release?.url || !release?.signature) {
        throw new Error(`Platform ${target} needs both a URL and a signature`);
      }
      platforms[target] = release;
    }
  }
  return { version, notes, pub_date: pubDate, platforms };
}

function parseArgs(argv) {
  const [command, ...rest] = argv;
  const args = { command, overlays: [], notes: "", repo: "FishwithCat/Oaklet" };
  for (let index = 0; index < rest.length; index += 1) {
    const arg = rest[index];
    if (arg === "--overlay") {
      index += 1;
      while (index < rest.length && !rest[index].startsWith("--")) {
        args.overlays.push(rest[index]);
        index += 1;
      }
      index -= 1;
    } else if (arg === "--base") {
      args.base = rest[++index];
    } else if (arg === "--output") {
      args.output = rest[++index];
    } else if (arg === "--version") {
      args.version = rest[++index];
    } else if (arg === "--platform") {
      args.platform = rest[++index];
    } else if (arg === "--artifact") {
      args.artifact = rest[++index];
    } else if (arg === "--signature") {
      args.signature = rest[++index];
    } else if (arg === "--repo") {
      args.repo = rest[++index];
    } else if (arg === "--notes") {
      args.notes = rest[++index];
    } else if (arg === "--pub-date") {
      args.pubDate = rest[++index];
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  if (!args.output) throw new Error("--output is required");
  return args;
}

function writeManifest(path, manifest) {
  mkdirSync(dirname(resolve(path)), { recursive: true });
  writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.command === "fragment") {
    writeManifest(
      args.output,
      makeFragment({
        version: args.version,
        platform: args.platform,
        artifact: args.artifact,
        signature: args.signature,
        repo: args.repo,
        notes: args.notes,
        pubDate: args.pubDate,
      }),
    );
    return;
  }
  if (args.command === "merge") {
    const read = (path) => (path ? JSON.parse(readFileSync(path, "utf8")) : null);
    writeManifest(
      args.output,
      mergeManifests(
        read(args.base),
        args.overlays.map((path) => read(path)),
      ),
    );
    return;
  }
  throw new Error("Use `fragment` or `merge`");
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    main();
  } catch (error) {
    console.error(`latest-json: ${error.message}`);
    process.exit(1);
  }
}
