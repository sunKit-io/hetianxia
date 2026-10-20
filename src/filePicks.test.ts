import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { invertPicked, pickedFromSkipped, skippedOf } from "./filePicks.ts";

test("unchecked relative paths come back after a rescan and invert flips files", () => {
  const first = ["readme.txt", "src/new.ts", "src/util/c.ts"];
  const picked = new Set(["readme.txt", "src/util/c.ts"]);
  assert.deepEqual(skippedOf(first, picked), ["src/new.ts"]);
  const rescanned = ["readme.txt", "src/new.ts", "src/util/c.ts", "extra.txt"];
  const restored = pickedFromSkipped(rescanned, ["src/new.ts", "gone.ts"]);
  assert.deepEqual([...restored], ["readme.txt", "src/util/c.ts", "extra.txt"]);
  const flipped = invertPicked(rescanned, restored);
  assert.deepEqual([...flipped], ["src/new.ts"]);
  assert.deepEqual(skippedOf(rescanned, flipped), ["readme.txt", "src/util/c.ts", "extra.txt"]);
});

test("preview stores the skipped set and offers 反选", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");
  assert.match(app, /反选/);
  assert.match(app, /invertPicked\(allPaths, picked\)/);
  assert.match(app, /loadFilePicks\(project, source\)/);
  assert.match(app, /saveFilePicks\(projectRoot, packageSource, skippedOf\(/);
  assert.match(app, /pickedFromSkipped\(/);
  assert.match(backend, /"load_file_picks"/);
  assert.match(backend, /"save_file_picks"/);
  assert.equal(backend.includes("db_path"), false);
});
