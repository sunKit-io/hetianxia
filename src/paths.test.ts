import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("the frontend does not send db or backup paths", () => {
  const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");
  assert.equal(backend.includes("dbPath"), false);
  assert.equal(backend.includes("backupRoot"), false);
  assert.equal(backend.includes("db_path"), false);
  assert.equal(backend.includes("backup_root"), false);
  assert.match(backend, /"resume_run", \{ execId, choice \}/);
  assert.match(backend, /"rollback_run", \{ execId, choice \}/);
  assert.match(backend, /"startup_failure"/);
});

test("a begin_run failure opens a dialog", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  assert.match(app, /startupFailure\(\)/);
  assert.match(app, /role="alertdialog"/);
  assert.match(app, /启动失败/);
});
