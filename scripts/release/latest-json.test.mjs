import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  assetUrl,
  makeFragment,
  mergeManifests,
  signatureFor,
} from "./latest-json.mjs";

test("asset URLs point at the release tag and encode the filename", () => {
  assert.equal(
    assetUrl({
      repo: "FishwithCat/Oaklet",
      version: "0.4.0",
      artifact: "Oaklet 0.4.0 aarch64.app.tar.gz",
    }),
    "https://github.com/FishwithCat/Oaklet/releases/download/v0.4.0/Oaklet%200.4.0%20aarch64.app.tar.gz",
  );
});

test("fragments read the signature file and trim it", () => {
  const root = mkdtempSync(join(tmpdir(), "oaklet-manifest-"));
  try {
    const signature = join(root, "artifact.sig");
    writeFileSync(signature, "  signature-value\n");
    assert.equal(signatureFor(signature), "signature-value");
    const fragment = makeFragment({
      version: "0.4.0",
      platform: "darwin-aarch64",
      artifact: "Oaklet.app.tar.gz",
      signature,
      repo: "FishwithCat/Oaklet",
      pubDate: "2026-01-01T00:00:00Z",
    });
    assert.equal(fragment.platforms["darwin-aarch64"].signature, "signature-value");
    assert.equal(
      fragment.platforms["darwin-aarch64"].url,
      "https://github.com/FishwithCat/Oaklet/releases/download/v0.4.0/Oaklet.app.tar.gz",
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("merging keeps every platform and rejects mixed versions", () => {
  const base = {
    version: "0.4.0",
    notes: "notes",
    pub_date: "2026-01-01T00:00:00Z",
    platforms: {
      "darwin-aarch64": { url: "https://example.test/a", signature: "a" },
    },
  };
  const overlay = {
    version: "0.4.0",
    notes: "notes",
    pub_date: "2026-01-02T00:00:00Z",
    platforms: {
      "windows-x86_64": { url: "https://example.test/w", signature: "w" },
    },
  };
  const merged = mergeManifests(base, [overlay]);
  assert.deepEqual(Object.keys(merged.platforms).sort(), [
    "darwin-aarch64",
    "windows-x86_64",
  ]);
  assert.equal(merged.pub_date, "2026-01-02T00:00:00Z");
  assert.throws(
    () => mergeManifests(base, [{ ...overlay, version: "0.5.0" }]),
    /Refusing to mix versions/,
  );
});
