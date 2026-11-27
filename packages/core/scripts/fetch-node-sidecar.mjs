#!/usr/bin/env node
// Download the pinned Node.js runtime that Oaklet bundles as a sidecar.
//
// The file name follows Tauri's externalBin convention:
// binaries/oaklet-node-<target-triple>[.exe]. The binary is not committed;
// `pnpm tauri build` (and the release workflows) fetch it once per target.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const NODE_VERSION = "22.23.3";
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BINARIES = join(ROOT, "binaries");

const TARGETS = {
  "aarch64-apple-darwin": { distro: "darwin-arm64", binary: "node" },
  "x86_64-apple-darwin": { distro: "darwin-x64", binary: "node" },
  "x86_64-pc-windows-msvc": { distro: "win-x64", binary: "node.exe" },
  "aarch64-pc-windows-msvc": { distro: "win-arm64", binary: "node.exe" },
  "x86_64-unknown-linux-gnu": { distro: "linux-x64", binary: "node" },
  "aarch64-unknown-linux-gnu": { distro: "linux-arm64", binary: "node" },
};

function hostTriple() {
  try {
    const triple = execFileSync("rustc", ["--print", "host-tuple"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (triple) return triple;
  } catch {
    // Fall through to the platform mapping below.
  }
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "darwin") {
    return arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  }
  if (platform === "win32") {
    return arch === "arm64"
      ? "aarch64-pc-windows-msvc"
      : "x86_64-pc-windows-msvc";
  }
  if (platform === "linux") {
    return arch === "arm64"
      ? "aarch64-unknown-linux-gnu"
      : "x86_64-unknown-linux-gnu";
  }
  return `${arch}-unknown-${platform}`;
}

function parseArgs(argv) {
  const args = { target: process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple(), force: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--force") {
      args.force = true;
    } else if (arg === "--target") {
      const value = argv[index + 1];
      if (!value) throw new Error("--target needs a Rust target triple");
      args.target = value;
      index += 1;
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return args;
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function findFile(root, name) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(path, name);
      if (found) return found;
    } else if (entry.name === name) {
      return path;
    }
  }
  return null;
}

async function download(url, destination) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`GET ${url} failed with HTTP ${response.status}`);
  }
  writeFileSync(destination, Buffer.from(await response.arrayBuffer()));
}

async function main() {
  const { target, force } = parseArgs(process.argv.slice(2));
  const spec = TARGETS[target];
  if (!spec) {
    throw new Error(
      `No Node.js ${NODE_VERSION} distribution is mapped for ${target}.`,
    );
  }

  mkdirSync(BINARIES, { recursive: true });
  const extension = target.includes("windows") ? ".exe" : "";
  const output = join(BINARIES, `oaklet-node-${target}${extension}`);
  if (!force && existsSync(output) && statSync(output).size > 1_000_000) {
    console.log(`Using existing Node.js sidecar: ${output}`);
    return;
  }

  const archiveName = `node-v${NODE_VERSION}-${spec.distro}.${spec.distro.startsWith("win") ? "zip" : "tar.gz"}`;
  const base = `https://nodejs.org/dist/v${NODE_VERSION}`;
  const work = mkdtempSync(join(tmpdir(), "oaklet-node-"));
  const archive = join(work, archiveName);
  try {
    console.log(`Downloading ${archiveName}...`);
    const sums = await fetch(`${base}/SHASUMS256.txt`, { redirect: "follow" });
    if (!sums.ok) {
      throw new Error(`Could not read Node.js checksums: HTTP ${sums.status}`);
    }
    const expected = (await sums.text())
      .split("\n")
      .map((line) => line.trim().split(/\s+/))
      .find(([, name]) => name === archiveName)?.[0];
    if (!expected) {
      throw new Error(`Node.js did not publish a checksum for ${archiveName}`);
    }

    await download(`${base}/${archiveName}`, archive);
    const actual = sha256(archive);
    if (actual !== expected) {
      throw new Error(
        `Checksum mismatch for ${archiveName}: expected ${expected}, got ${actual}`,
      );
    }

    const extracted = join(work, "extract");
    mkdirSync(extracted, { recursive: true });
    execFileSync("tar", ["-xf", archive, "-C", extracted], { stdio: "inherit" });
    const binary = findFile(extracted, spec.binary);
    if (!binary) {
      throw new Error(`${spec.binary} was not found in ${archiveName}`);
    }
    copyFileSync(binary, output);
    if (!extension) chmodSync(output, 0o755);
    console.log(`Node.js v${NODE_VERSION} sidecar ready: ${output}`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`Could not prepare the Node.js sidecar: ${error.message}`);
  process.exit(1);
});
