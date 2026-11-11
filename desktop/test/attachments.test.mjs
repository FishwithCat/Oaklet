import assert from "node:assert/strict";
import test from "node:test";
import {
  formatBytes,
  sentAttachments,
  userContent,
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

test("text without attachments stays a plain string", () => {
  assert.equal(userContent("hello", []), "hello");
});

test("images become image parts after the typed text", () => {
  assert.deepEqual(userContent("look", [image]), [
    { type: "text", text: "look" },
    { type: "image_url", image_url: { url: image.dataUrl } },
  ]);
});

test("an image-only message has no empty text part", () => {
  assert.deepEqual(userContent("", [image]), [
    { type: "image_url", image_url: { url: image.dataUrl } },
  ]);
});

test("text files are inlined with their name", () => {
  const parts = userContent("read this", [file]);
  assert.equal(parts.length, 2);
  assert.equal(parts[0].text, "read this");
  assert.match(parts[1].text, /^Attached file notes\.md:\n\n# Notes/);
});

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
