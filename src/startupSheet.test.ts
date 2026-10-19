import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("startup sheet stays closed until a real offer exists", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  assert.match(app, /useState<Sheet>\(null\)/);
  assert.equal(app.includes('useState<Sheet>("startup")'), false);
  assert.match(app, /else setSheet\("startup"\)/);
  assert.equal(app.includes("const STOPPED"), false);
  assert.equal(app.includes("ROLLBACK_FILES"), false);
  assert.equal(app.includes("10月12日"), false);
  assert.equal(app.includes("已恢复 1 / 3"), false);
  assert.equal(app.includes("No space left on device"), false);
  assert.equal(app.includes("日志失败示例"), false);
  assert.equal(app.includes("9 / 20"), false);
  const title = app.slice(app.indexOf("titleblock"), app.indexOf("pages"));
  assert.equal(title.includes("OrderCenter"), false);
  assert.equal(title.includes("src/config/app.config.js"), false);
});

test("rollback preview loads that exec before the sheet opens", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");
  const open = app.slice(app.indexOf("async function openRollback"));
  const previewAt = open.indexOf("rollbackPreview(execId)");
  const filesAt = open.indexOf("setRollbackFiles(preview.files)");
  const sheetAt = open.indexOf('setSheet("rollback")');
  assert.ok(previewAt >= 0 && filesAt > previewAt && sheetAt > filesAt);
  assert.match(app, /openRollback\(execId\)/);
  assert.match(app, /openRollback\(offer\.execId\)/);
  assert.match(backend, /"rollback_preview", \{ execId \}/);
  assert.equal(backend.includes("dbPath"), false);
  assert.equal(backend.includes("backupRoot"), false);
});
