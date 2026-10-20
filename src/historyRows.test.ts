import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { formatFileCounts, historyContinue, historyRowButtons, historyStatusLabel, REMOTE_ROLLBACK_TITLE } from "./historyRows.ts";

test("history status labels and buttons follow the journal state", () => {
  assert.equal(historyStatusLabel("COMMITTED"), "已完成");
  assert.equal(historyStatusLabel("STOPPED"), "部分失败");
  assert.equal(historyStatusLabel("PREPARED"), "未完成");
  assert.equal(historyStatusLabel("BACKED_UP"), "未完成");
  assert.equal(historyStatusLabel("VERIFYING"), "未完成");
  assert.equal(historyStatusLabel("LOG_FAILED"), "未完成");
  assert.equal(historyStatusLabel("ROLLED_BACK"), "已回滚");
  assert.equal(historyStatusLabel("ROLLBACK_PARTIAL"), "部分回滚");
  assert.equal(historyStatusLabel("ROLLING_BACK"), "未知状态");
  assert.equal(historyStatusLabel("OTHER"), "未知状态");

  assert.deepEqual(historyRowButtons("未完成"), { rollback: true, resume: true });
  assert.deepEqual(historyRowButtons("部分失败"), { rollback: true, resume: true });
  assert.deepEqual(historyRowButtons("已完成"), { rollback: true, resume: false });
  assert.deepEqual(historyRowButtons("部分回滚"), { rollback: true, resume: false });
  assert.deepEqual(historyRowButtons("已回滚"), { rollback: false, resume: false });
  assert.deepEqual(historyRowButtons("未知状态"), { rollback: false, resume: false });

  const first = historyContinue("exec-row-1");
  const second = historyContinue("exec-row-2");
  assert.equal(first.command, "resume_run");
  assert.equal(first.execId, "exec-row-1");
  assert.equal(first.choice, "continue");
  assert.notEqual(first.execId, second.execId);
  assert.equal(REMOTE_ROLLBACK_TITLE, "远程写入暂不支持回滚");
  assert.equal(formatFileCounts({ fileCount: 4, doneCount: 2, failedCount: 1 }), "共 4 · 已写 2 · 失败 1");
  assert.equal(formatFileCounts({ fileCount: 4, doneCount: 2, failedCount: 0, conflictCount: 2 }), "共 4 · 已写 2 · 失败 0 · 冲突 2");
});

test("history rollback opens the sheet for that row and does not call rollback_run", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const rows = readFileSync(new URL("./historyRows.ts", import.meta.url), "utf8");
  assert.equal(app.includes("const HISTORY"), false);
  assert.equal(app.includes("HeTianXia"), false);
  assert.equal(app.includes("historyRollback"), false);
  assert.equal(rows.includes("proceedBackupCurrent"), false);
  assert.match(app, /setFlowId\(execId\)/);
  assert.match(app, /setSheet\("rollback"\)/);
  assert.match(app, /onRollback\(run\.execId\)/);
  assert.match(app, /historyContinue\(run\.execId\)/);
  assert.match(app, /historyStatusLabel\(run\.status\)/);
  const history = app.slice(app.indexOf("function HistoryPage"), app.indexOf("function VerifyFailedSheet"));
  assert.match(history, /\[refreshToken\]/);
  const rollbackStart = app.indexOf("async function applyRollback");
  const rollback = app.slice(rollbackStart, app.indexOf("return (", rollbackStart));
  assert.match(rollback, /setHistoryToken\(\(token\) => token \+ 1\)/);
  assert.ok(rollback.indexOf("rollbackRun") < rollback.indexOf("setHistoryToken"));
  assert.match(app, /title=\{REMOTE_ROLLBACK_TITLE\}/);
  assert.match(app, /applyResume\(offer\.execId/);
  assert.match(app, /applyRollback\(offer\.execId/);
});
