import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  MENTION_COMMANDS,
  activeMention,
  insertMention,
  mentionOptions,
} from "../src/features/chat/mentions.ts";

test("opening a mention at the start or after whitespace", () => {
  assert.deepEqual(activeMention("@", 1), { start: 0, end: 1, query: "" });
  assert.deepEqual(activeMention("please @ar", 10), {
    start: 7,
    end: 10,
    query: "ar",
  });
  assert.equal(activeMention("dev@arch.example", 10), null);
  assert.equal(activeMention("plain text", 10), null);
});

test("completion filters by prefix and matches every command at @", () => {
  assert.deepEqual(
    mentionOptions("").map((command) => command.name),
    MENTION_COMMANDS.map((command) => command.name),
  );
  assert.deepEqual(
    mentionOptions("AR").map((command) => command.name),
    ["arch"],
  );
  assert.deepEqual(mentionOptions("nope"), []);
});

test("inserting a command replaces the typed token and leaves a space", () => {
  const mention = activeMention("plan @ar a worker", 8);
  assert.ok(mention);
  assert.deepEqual(insertMention("plan @ar a worker", mention, "arch"), {
    value: "plan @arch a worker",
    caret: 11,
  });
  // Text right of the caret belongs to the token and is replaced too.
  const inside = activeMention("@arch worker", 4);
  assert.ok(inside);
  assert.deepEqual(insertMention("@arch worker", inside, "arch"), {
    value: "@arch worker",
    caret: 6,
  });
});

test("the desktop catalog matches the backend registry", () => {
  const rust = readFileSync(
    new URL("../../core/src/chat/mentions.rs", import.meta.url),
    "utf8",
  );
  const names = [...rust.matchAll(/name: "([a-z0-9-]+)"/g)].map(
    (match) => match[1],
  );
  assert.deepEqual(
    MENTION_COMMANDS.map((command) => command.name).sort(),
    names.sort(),
  );
});
