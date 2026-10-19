import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { formatFileCounts, historyContinue, historyRollback, REMOTE_ROLLBACK_TITLE } from "./historyRows.ts";

test("a history row button passes that row's exec id", () => {
  const first = "exec-row-1";
  const second = "exec-row-2";
  const rollback = historyRollback(first);
  const otherRollback = historyRollback(second);
  assert.equal(rollback.command, "rollback_run");
  assert.equal(rollback.execId, first);
  assert.equal(rollback.choice, "proceedBackupCurrent");
  assert.equal(otherRollback.execId, second);
  assert.notEqual(rollback.execId, otherRollback.execId);

  const resume = historyContinue(first);
  const otherResume = historyContinue(second);
  assert.equal(resume.command, "resume_run");
  assert.equal(resume.execId, first);
  assert.equal(resume.choice, "continue");
  assert.equal(otherResume.execId, second);
  assert.notEqual(resume.execId, otherResume.execId);
  assert.equal(REMOTE_ROLLBACK_TITLE, "远程写入暂不支持回滚");
  assert.equal(formatFileCounts({ fileCount: 4, doneCount: 2, failedCount: 1 }), "共 4 · 已写 2 · 失败 1");
});

test("the history page uses the clicked row and the startup prompt uses its own id", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  assert.equal(app.includes("const HISTORY"), false);
  assert.equal(app.includes("远程写入还没支持"), false);
  assert.match(app, /historyRollback\(run\.execId\)/);
  assert.match(app, /historyContinue\(run\.execId\)/);
  assert.match(app, /title=\{REMOTE_ROLLBACK_TITLE\}/);
  assert.match(app, /applyResume\(offer\.execId/);
  assert.match(app, /applyRollback\(offer\.execId/);
  assert.match(app, /applyRollback\(action\.execId, action\.choice\)/);
  assert.match(app, /applyResume\(action\.execId, action\.choice\)/);
});
