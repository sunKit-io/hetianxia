import assert from "node:assert/strict";
import test from "node:test";
import { afterResume, engineCode, formatEngineError, formatFileResult, formatOffer, formatReport, parseResume, type ExecReport } from "./engine.ts";

const stopped: ExecReport = {
  execId: "exec-1",
  state: "STOPPED",
  files: [{ index: 3, path: "src/config/app.config.js", kind: "overwrite", state: "failed", error: "写入失败", code: "E_IO" }],
  backupRoot: "/bak",
  logStopped: false,
  historyResult: "部分失败",
  sheetActions: ["回滚", "稍后处理"],
};

test("engine codes use the E_ names", () => {
  assert.equal(engineCode("IO"), "E_IO");
  assert.equal(engineCode("VERIFY_FAILED"), "E_VERIFY_FAILED");
  assert.equal(engineCode("INVALID_STATE"), "E_INVALID_STATE");
  assert.equal(engineCode("CHANGED_SINCE_SCAN"), "E_CHANGED_SINCE_SCAN");
  assert.equal(engineCode("E_IO"), "E_IO");
});

test("command errors show the code and message", () => {
  assert.equal(formatEngineError({ code: "VERIFY_FAILED", message: "校验失败" }), "E_VERIFY_FAILED · 校验失败");
  assert.equal(formatEngineError({ code: "INVALID_STATE", message: "这次升级已经结束，不能继续" }), "E_INVALID_STATE · 这次升级已经结束，不能继续");
  assert.equal(formatEngineError({ code: "CHANGED_SINCE_SCAN", message: "扫描后内容已变" }), "E_CHANGED_SINCE_SCAN · 扫描后内容已变");
  assert.equal(formatEngineError({ code: "CANCELLED", message: "已取消回滚" }), "E_CANCELLED · 已取消回滚");
});

test("reports show STOPPED, ROLLED_BACK, and ROLLBACK_PARTIAL", () => {
  assert.match(formatReport(stopped), /^STOPPED · 部分失败/);
  assert.match(formatReport(stopped), /src\/config\/app\.config\.js · E_IO · 写入失败/);
  assert.equal(
    formatReport({ ...stopped, state: "ROLLED_BACK", historyResult: "已回滚", files: [] }),
    "ROLLED_BACK · 已回滚",
  );
  assert.equal(
    formatReport({ ...stopped, state: "ROLLBACK_PARTIAL", historyResult: "部分回滚", files: [{ index: 1, path: "src/routes.js", kind: "overwrite", state: "skipped", code: "E_CHANGED_SINCE_SCAN", error: "升级后被修改" }] }),
    "ROLLBACK_PARTIAL · 部分回滚\nsrc/routes.js · E_CHANGED_SINCE_SCAN · 升级后被修改",
  );
});

test("resume outcomes pick the sheet from the engine", () => {
  assert.deepEqual(afterResume("cancel", parseResume("unchanged")), { sheet: null, status: null });
  assert.deepEqual(afterResume("continue", parseResume({ continued: stopped })).sheet, null);
  assert.match(afterResume("continue", parseResume({ continued: stopped })).status ?? "", /STOPPED/);
  const external = afterResume("continue", parseResume({ rollbackPrompt: { title: "有 1 个已经写完的文件被改过", files: [{ path: "src/routes.js", detail: "升级后被修改" }] } }));
  assert.equal(external.sheet, "external");
  if (external.sheet === "external") assert.equal(external.files[0].path, "src/routes.js");
  const rollback = afterResume("rollback", parseResume({ rollbackPrompt: { title: "回滚", files: [{ path: "a.txt", detail: "升级后被删除" }] } }));
  assert.equal(rollback.sheet, "rollback");
  const pending = afterResume("keepExternal", parseResume({ changesSinceScan: [{ path: "config/local.env", hashChanged: true, detail: "升级后被修改" }] }));
  assert.equal(pending.sheet, "pending");
  if (pending.sheet === "pending") assert.equal(pending.files[0].path, "config/local.env");
});

test("file results use Chinese states and keep the error code", () => {
  assert.equal(formatFileResult({ state: "done", code: null, error: null }), "已写入");
  assert.equal(formatFileResult({ state: "skipped", code: "E_PERMISSION", error: null }), "已跳过 · E_PERMISSION");
  assert.equal(formatFileResult({ state: "failed", code: "E_IO", error: "写入失败" }), "失败 · E_IO · 写入失败");
  assert.equal(formatFileResult({ state: "doing", code: null, error: null }), "进行中");
});

test("startup offer shows the state and code", () => {
  assert.equal(formatOffer({ execId: "e", state: "STOPPED", preRecoveryState: "STOPPED", actions: ["继续"], conflicts: [], code: "IO", error: "写入失败" }), "STOPPED · E_IO · 写入失败");
  assert.equal(formatOffer({ execId: "e", state: "ROLLBACK_PARTIAL", preRecoveryState: "ROLLING_BACK", actions: ["继续回滚"], conflicts: [] }), "ROLLBACK_PARTIAL");
});
