import assert from "node:assert/strict";
import test from "node:test";
import {
  ATTR_REASON,
  ATTR_UNRESTORED,
  CANCEL_FOOTER,
  DETAIL_DELETED,
  DETAIL_MODIFIED,
  OVERWRITE_PENDING,
  OVERWRITE_PENDING_NOTE,
  PARTIAL_ROLLBACK_CHANGE,
  PARTIAL_ROLLBACK_RESULT,
  PARTIAL_ROLLBACK_UNRESTORED,
  RECREATED_REASON,
  RESUME_AGAIN,
  RESUME_NOTE,
  ROLLBACK_RESUME_BODY,
  SKIP_PENDING,
  STOP_BODY,
  STOP_FOOTER,
  attrRestoredNote,
  backupCheckboxLabel,
  isDeletedDetail,
  keepExternalNote,
  partialRollbackIntro,
  pendingChangedTitle,
  resumeConflictScreen,
  resumeConflictTitle,
  resumeExplain,
  rollbackConflictSubtitle,
  rollbackProceedLabel,
  rollbackProceedNote,
  rollbackSkipNote,
  skipPendingNote,
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
  assert.equal(isDeletedDetail("升级后被删除 · 今天 13:57"), true);
  assert.equal(isDeletedDetail("升级后修改 · 今天 13:58"), false);
});

test("pending overwrite and rollback copy stay pinned", () => {
  assert.equal(pendingChangedTitle(2), "有 2 个还没写的文件被改过");
  assert.equal(SKIP_PENDING, "跳过这些文件，继续其余的");
  assert.equal(skipPendingNote(2), "这 2 个文件保持你改过的样子，其余文件照常写入。");
  assert.equal(OVERWRITE_PENDING, "仍然覆盖");
  assert.equal(OVERWRITE_PENDING_NOTE, "先把你改过的当前版本重新备份并校验，再覆盖。回滚时恢复的就是这一版。");
  assert.equal(rollbackProceedLabel(true), "继续回滚");
  assert.equal(rollbackProceedNote(2, 1), "被改过的 2 个先备份当前版本再覆盖；被删除的 1 个用备份恢复成升级前的版本");
  assert.equal(rollbackSkipNote(3, 1), "其余文件照常回滚，这 3 个保持现状，被删除的继续保持删除");
  assert.equal(rollbackProceedLabel(false), "继续回滚（先备份当前版本）");
  assert.equal(rollbackProceedNote(2, 0), "先把这些文件的当前版本备份，再覆盖回升级前的样子");
  assert.equal(rollbackSkipNote(2, 0), "其余文件照常回滚，这些保持现状");
  assert.equal(rollbackConflictSubtitle(true), "这些文件在 10月12日 升级后又被改动或删除过");
  assert.equal(RESUME_AGAIN, "下次启动还会再提示");
  assert.equal(CANCEL_FOOTER, "选「取消」什么都不做，下次启动还会再提示。");
  assert.equal(STOP_BODY, "已停止写入后面的文件，不会自动回滚");
  assert.equal(STOP_FOOTER, "选「稍后处理」后，下次启动会继续提示这次未完成的升级。历史记录会标成「部分失败」。");
  assert.equal(RESUME_NOTE, resumeExplain(9));
  assert.equal(keepExternalNote(2, false, "src/config/app.yaml"), "这 2 个文件不再动，从失败的 src/config/app.yaml 接着写。以后回滚时它们会按冲突处理。");
  assert.equal(keepExternalNote(2, true, "src/config/app.yaml"), "这 2 个文件不再动（被删除的保持删除），从失败的 src/config/app.yaml 接着写。以后回滚时它们会按冲突处理。");
  assert.equal(ROLLBACK_RESUME_BODY, "回滚做到一半时被中断了。为了避免已经还原的文件被重新写掉，这里只能接着回滚，不能再继续升级。");
  assert.equal(RECREATED_REASON, "原位置被重新创建，和备份不同");
});

test("partial rollback history lists only the files that stayed unrestored", () => {
  assert.equal(PARTIAL_ROLLBACK_RESULT, "部分回滚");
  assert.equal(PARTIAL_ROLLBACK_CHANGE, "已恢复 7 / 9 · 跳过 2");
  assert.equal(partialRollbackIntro(2), "这 2 个文件没有恢复，保持回滚前的样子：");
  assert.deepEqual(
    PARTIAL_ROLLBACK_UNRESTORED.map((file) => `${file.path} · ${file.reason}`),
    ["src/routes.js · 升级后被修改，已跳过", "conf/legacy.yml · 被外部删除，已跳过"],
  );
  assert.equal(attrRestoredNote(2), "有 2 个文件内容已恢复，权限 / 修改时间未恢复");
  assert.equal(ATTR_REASON, "内容已恢复，权限 / 修改时间未恢复");
  assert.deepEqual(
    ATTR_UNRESTORED.map((file) => file.path),
    ["bin/deploy.sh", "conf/app.yml"],
  );
});

test("backup checkbox repeats the directory it is given", () => {
  const fromShell = "~/Library/Application Support/com.hetianxia.app/bak/exec-9/";
  assert.equal(backupCheckboxLabel(fromShell), `执行前备份到 ${fromShell}`);
  assert.equal(backupCheckboxLabel("D:/bak/other"), "执行前备份到 D:/bak/other");
  assert.equal(backupCheckboxLabel(""), "执行前备份到 ");
});
