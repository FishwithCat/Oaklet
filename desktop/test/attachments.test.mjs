import assert from "node:assert/strict";
import test from "node:test";
import {
  formatBytes,
  sentAttachments,
} from "../src/features/chat/attachments.ts";

const image = {
  kind: "image",
  name: "shot.png",
  bytes: 2048,
  dataUrl: "data:image/jpeg;base64,AAAA",
  thumbnail: "data:image/jpeg;base64,BBBB",
};

const file = {
  kind: "file",
  name: "notes.md",
  bytes: 120,
  text: "# Notes\n\nShip it.",
};

test("sent attachments keep the thumbnail but drop file contents", () => {
  assert.deepEqual(sentAttachments([image, file]), [
    { kind: "image", name: "shot.png", bytes: 2048, thumbnail: image.thumbnail },
    { kind: "file", name: "notes.md", bytes: 120 },
  ]);
});

test("byte sizes stay short", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(2048), "2.0 KB");
  assert.equal(formatBytes(5 * 1024 * 1024), "5.0 MB");
});
