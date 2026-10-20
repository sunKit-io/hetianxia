import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { confirmExecuteBody, settleExecuteConfirm, type ExecuteConfirm } from "./confirmExecute.ts";

const path = "/data/bak/exec-1";

function preview(create: number, overwrite: number, moveCount: number): Pick<ExecuteConfirm, "create" | "overwrite" | "moveCount" | "backupPath"> {
  return { create, overwrite, moveCount, backupPath: path };
}

test("confirm copy shows the backend counts and backup path, and drops zero kinds", () => {
  assert.equal(
    confirmExecuteBody(preview(2, 1, 3)),
    "将新增 2 个、覆盖 1 个、移动 3 个文件（保留旧文件）。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(2, 1, 0)),
    "将新增 2 个、覆盖 1 个文件。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(2, 0, 3)),
    "将新增 2 个、移动 3 个文件（保留旧文件）。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(0, 1, 3)),
    "将覆盖 1 个、移动 3 个文件（保留旧文件）。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(4, 0, 0)),
    "将新增 4 个文件。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(0, 5, 0)),
    "将覆盖 5 个文件。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  assert.equal(
    confirmExecuteBody(preview(0, 0, 6)),
    "将移动 6 个文件（保留旧文件）。执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。",
  );
  const none = confirmExecuteBody(preview(0, 0, 0));
  assert.equal(none, "执行前会把原文件备份到 /data/bak/exec-1，出问题可以一键回滚。");
  assert.equal(none.includes("新增"), false);
  assert.equal(none.includes("覆盖"), false);
  assert.equal(none.includes("移动"), false);
});

test("cancel and escape do not execute; confirm does", () => {
  let writes = 0;
  const run = () => { writes += 1; };
  assert.equal(settleExecuteConfirm("cancel", run), "cancelled");
  assert.equal(settleExecuteConfirm("escape", run), "cancelled");
  assert.equal(writes, 0);
  assert.equal(settleExecuteConfirm("confirm", run), "ran");
  assert.equal(writes, 1);
});

test("the confirm sheet shows the backend sentence, focuses cancel, and escape cancels", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const sheet = app.slice(app.indexOf("function ExecuteConfirmSheet"), app.indexOf("function HistoryPage"));
  assert.ok(sheet.includes("function ExecuteConfirmSheet"));
  assert.match(sheet, /确认执行升级/);
  assert.match(sheet, /confirmExecuteBody\(preview\)/);
  assert.match(sheet, /preview\.conflicts/);
  assert.match(sheet, /file\.detail/);
  assert.equal(sheet.includes("不会覆盖"), false);
  assert.equal(sheet.includes("已经存在"), false);
  assert.equal(sheet.includes("previewCounts"), false);
  assert.equal(sheet.includes("executeLocal"), false);
  const cancelAt = sheet.indexOf(">取消<");
  const proceedAt = sheet.indexOf(">确认执行<");
  const focusAt = sheet.indexOf("autoFocus");
  assert.ok(focusAt >= 0 && focusAt < cancelAt && cancelAt < proceedAt);
  assert.match(sheet, /className="primary"/);
  assert.match(sheet, /Escape/);
  assert.match(sheet, /cancelRef\.current\?\.focus\(\)/);
  const flow = app.slice(app.indexOf("async function backupAndExecute"), app.indexOf("async function applyResume"));
  const chosenAt = flow.indexOf("selectedPlan(plan, pickedPaths)");
  const execPlanAt = flow.indexOf("execPlanFor(chosen,");
  const confirmCallAt = flow.indexOf("confirmExecute(");
  assert.ok(chosenAt >= 0 && chosenAt < execPlanAt && execPlanAt < confirmCallAt);
  assert.equal(flow.includes("previewCounts"), false);
  assert.match(flow, /settleExecuteConfirm\("cancel"/);
  assert.match(flow, /settleExecuteConfirm\("escape"/);
  assert.match(flow, /settleExecuteConfirm\("confirm"/);
  const executeAt = flow.indexOf("executeLocal(");
  const confirmAt = flow.indexOf('settleExecuteConfirm("confirm"');
  assert.ok(executeAt > confirmAt);
  assert.equal(flow.slice(0, confirmAt).includes("executeLocal("), false);
});
