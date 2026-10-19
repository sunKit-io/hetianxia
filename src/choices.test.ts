import assert from "node:assert/strict";
import test from "node:test";
import { externalChoice, keepSource, pendingChoice, rollbackChoice } from "./choices.ts";

test("备份当前 and 跳过 map to rollback choices", () => {
  assert.equal(rollbackChoice("proceed"), "proceedBackupCurrent");
  assert.equal(rollbackChoice("skip"), "skipConflicts");
  assert.equal(rollbackChoice("cancel"), "cancel");
});

test("保留外部修改并继续 maps to keepExternal", () => {
  assert.equal(externalChoice("keep"), "keepExternal");
  assert.equal(externalChoice("rollback"), "rollback");
});

test("pending files map to skip or overwrite", () => {
  assert.equal(pendingChoice("skip"), "skipChanged");
  assert.equal(pendingChoice("overwrite"), "overwriteChanged");
});

test("keep_source is only the move that keeps the old file", () => {
  assert.equal(keepSource("移动（保留旧文件）"), true);
  assert.equal(keepSource("移动"), false);
});
