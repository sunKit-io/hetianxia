import assert from "node:assert/strict";
import test from "node:test";
import {
  DETAIL_DELETED,
  DETAIL_MODIFIED,
  OVERWRITE_PENDING,
  OVERWRITE_PENDING_NOTE,
  RESUME_AGAIN,
  RESUME_NOTE,
  SKIP_PENDING,
  STOP_BODY,
  STOP_FOOTER,
  pendingChangedTitle,
  resumeConflictScreen,
  resumeConflictTitle,
  rollbackProceedLabel,
  rollbackProceedNote,
  rollbackSkipNote,
} from "./resumeCopy.ts";

test("title switches when any written file was deleted", () => {
  const modified = { path: "src/routes.js", detail: DETAIL_MODIFIED };
  const deleted = { path: "public/old-banner.png", detail: DETAIL_DELETED };
  assert.equal(resumeConflictTitle([modified]), "有 1 个已经写完的文件被改过");
  assert.equal(resumeConflictTitle([modified, deleted]), "有 2 个已经写完的文件被改过或删除");
  assert.equal(resumeConflictScreen([modified]), "21");
  assert.equal(resumeConflictScreen([modified, deleted]), "23");
  assert.equal(DETAIL_MODIFIED, "升级后被修改");
  assert.equal(DETAIL_DELETED, "升级后被删除");
});

test("pending overwrite and rollback copy stay pinned", () => {
  assert.equal(pendingChangedTitle(2), "还有 2 个还没写的文件被改过");
  assert.equal(SKIP_PENDING, "跳过这些文件，继续其余的");
  assert.equal(OVERWRITE_PENDING, "仍然覆盖");
  assert.equal(OVERWRITE_PENDING_NOTE, "先重新备份并校验你现在的版本，通过之后才覆盖");
  assert.equal(rollbackProceedLabel(true), "继续回滚");
  assert.equal(rollbackProceedNote(true), "从备份恢复，删除的文件会重新出现");
  assert.equal(rollbackSkipNote(true), "保持现状。已经删除的文件不会恢复");
  assert.equal(rollbackProceedLabel(false), "继续回滚（先备份当前版本）");
  assert.equal(RESUME_AGAIN, "下次启动还会再提示");
  assert.equal(STOP_BODY, "已停止写入后面的文件，不会自动回滚");
  assert.equal(STOP_FOOTER, "下次启动还会再提示。历史记录会标成「部分失败」。");
  assert.equal(RESUME_NOTE, "继续会先核对已写完的文件，沿用原来的事务和备份");
});
