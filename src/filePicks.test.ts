import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { invertPicked, pickedFromSkipped, skippedOf } from "./filePicks.ts";

test("rescan restores unchecked files and checks files that are new", () => {
  const first = ["readme.txt", "src/new.ts", "src/util/c.ts"];
  const picked = new Set(["readme.txt", "src/util/c.ts"]);
  const stored = skippedOf(first, picked);
  assert.deepEqual(stored, ["src/new.ts"]);
  const rescanned = ["readme.txt", "src/new.ts", "src/util/c.ts", "extra.txt"];
  const restored = pickedFromSkipped(rescanned, stored.concat("gone.ts"));
  assert.deepEqual([...restored], ["readme.txt", "src/util/c.ts", "extra.txt"]);
  const flipped = invertPicked(rescanned, restored);
  assert.deepEqual([...flipped], ["src/new.ts"]);
  assert.deepEqual(skippedOf(rescanned, flipped), ["readme.txt", "src/util/c.ts", "extra.txt"]);
});

test("a restarted app drops session checks and reads the stored set again", () => {
  const stored = ["src/new.ts"];
  const wiped = new Set<string>();
  assert.equal(wiped.size, 0);
  const paths = ["readme.txt", "src/new.ts", "extra.txt"];
  assert.deepEqual([...pickedFromSkipped(paths, stored)], ["readme.txt", "extra.txt"]);
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");
  assert.match(app, /skipped = await loadFilePicks\(project, source\)/);
  assert.match(app, /setPickedPaths\(pickedFromSkipped\(paths, skipped\)\)/);
  assert.match(app, /setPackageSource\(picked\.source\)/);
  assert.match(app, /refreshPlan\(projectRoot, picked\.prepared, picked\.source\)/);
  assert.match(backend, /source: picked, prepared: await preparePackage\(picked\)/);
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
