import assert from "node:assert/strict";
import test from "node:test";
import { parseIndexArgs, stateText, subscribeIndex, updateIndexServer } from "../src/features/settings/code-index-model.ts";

const deferred = () => { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; };
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("subscribe before snapshot, reject old revisions, release on unmount", async () => {
  const snapshot = deferred();
  const seen = [];
  let receive;
  let subscribed = false;
  let released = 0;
  const close = subscribeIndex(async (callback) => {
    receive = callback; subscribed = true; return () => released++;
  }, () => { assert.ok(subscribed); return snapshot.promise; }, (status) => seen.push(status.revision), assert.fail);
  await tick();
  receive({ revision: 3 });
  snapshot.resolve({ revision: 2 });
  await tick();
  receive({ revision: 3 });
  receive({ revision: 4 });
  assert.deepEqual(seen, [3, 4]);
  close();
  receive({ revision: 5 });
  assert.equal(released, 1);
  assert.deepEqual(seen, [3, 4]);
});

test("destroyed window releases late listener and never reads snapshot", async () => {
  const listening = deferred();
  let released = 0;
  const close = subscribeIndex(() => listening.promise, assert.fail, assert.fail, assert.fail);
  close();
  listening.resolve(() => released++);
  await tick();
  assert.equal(released, 1);
});

test("listener and snapshot errors surface; late snapshot does not update unmounted state", async () => {
  const failures = [];
  const close = subscribeIndex(async () => { throw new Error("closed"); }, assert.fail, assert.fail, (error) => failures.push(error.message));
  await tick();
  assert.deepEqual(failures, ["closed"]);
  close();
  const snapshot = deferred();
  let released = 0;
  const stop = subscribeIndex(async () => () => released++, () => snapshot.promise, assert.fail, assert.fail);
  await tick();
  stop();
  snapshot.resolve({ revision: 6 });
  await tick();
  assert.equal(released, 1);
});

test("configuration edits preserve the other language and pass literal arguments", () => {
  const config = { servers: { rust: { enabled: true }, typescript: { command: "ts-server", args: ["--stdio"] } } };
  const next = updateIndexServer(config, "rust", { enabled: false });
  assert.deepEqual(next.servers.typescript, config.servers.typescript);
  assert.equal(config.servers.rust.enabled, true);
  assert.deepEqual(parseIndexArgs('["--stdio", "a b", "$(literal)"]'), ["--stdio", "a b", "$(literal)"]);
  for (const value of ['{}', '[1]', 'null', '["\\u0000"]', '']) assert.throws(() => parseIndexArgs(value));
  assert.match(stateText("Ready"), /semantic completeness unknown/);
  assert.match(stateText("Queued"), /waiting for server resources/);
  assert.match(stateText({ Failed: "missing executable" }), /missing executable/);
});

test("snapshot failure surfaces while subscription remains available", async () => {
  let receive;
  const seen = [];
  const failures = [];
  const close = subscribeIndex(async (callback) => { receive = callback; return () => {}; }, async () => { throw new Error("not ready"); }, (status) => seen.push(status.revision), (error) => failures.push(error.message));
  await tick();
  assert.deepEqual(failures, ["not ready"]);
  receive({ revision: 1 });
  assert.deepEqual(seen, [1]);
  close();
});
