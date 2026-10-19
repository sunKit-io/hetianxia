/** 中断升级、回滚和历史的界面文案。和设计图 18–27 对齐。 */

export const DETAIL_MODIFIED = "升级后被修改";
export const DETAIL_DELETED = "升级后被删除";
export const RECREATED_REASON = "原位置被重新创建，和备份不同";

export type ResumeFile = { path: string; detail: string; deleted?: boolean };

export function isDeletedDetail(detail: string, deleted?: boolean): boolean {
  return deleted === true || detail === DETAIL_DELETED || detail.startsWith(`${DETAIL_DELETED}`);
}

export function resumeConflictTitle(files: ResumeFile[]): string {
  const n = files.length;
  if (files.some((file) => isDeletedDetail(file.detail, file.deleted))) {
    return `有 ${n} 个已经写完的文件被改过或删除`;
  }
  return `有 ${n} 个已经写完的文件被改过`;
}

/** 有删除走画面 23，否则走画面 21。 */
export function resumeConflictScreen(files: ResumeFile[]): "21" | "23" {
  return files.some((file) => isDeletedDetail(file.detail, file.deleted)) ? "23" : "21";
}

export function pendingChangedTitle(count: number): string {
  return `有 ${count} 个还没写的文件被改过`;
}

export const SKIP_PENDING = "跳过这些文件，继续其余的";
export const OVERWRITE_PENDING = "仍然覆盖";
export const OVERWRITE_PENDING_NOTE = "先把你改过的当前版本重新备份并校验，再覆盖。回滚时恢复的就是这一版。";

export function skipPendingNote(count: number): string {
  return `这 ${count} 个文件保持你改过的样子，其余文件照常写入。`;
}

export function keepExternalNote(count: number, hasDeletion: boolean, resumeFile: string): string {
  const stay = hasDeletion
    ? `这 ${count} 个文件不再动（被删除的保持删除）`
    : `这 ${count} 个文件不再动`;
  return `${stay}，从失败的 ${resumeFile} 接着写。以后回滚时它们会按冲突处理。`;
}

export const ROLLBACK_CHOICE_NOTE = "撤回这次升级，被改过的文件会让你选择怎么处理。";

export function rollbackProceedLabel(hasDeletion: boolean): string {
  return hasDeletion ? "继续回滚" : "继续回滚（先备份当前版本）";
}

export function rollbackProceedNote(modified: number, deleted: number): string {
  if (deleted > 0) {
    return `被改过的 ${modified} 个先备份当前版本再覆盖；被删除的 ${deleted} 个用备份恢复成升级前的版本`;
  }
  return "先把这些文件的当前版本备份，再覆盖回升级前的样子";
}

export function rollbackSkipNote(total: number, deleted: number): string {
  if (deleted > 0) {
    return `其余文件照常回滚，这 ${total} 个保持现状，被删除的继续保持删除`;
  }
  return "其余文件照常回滚，这些保持现状";
}

export function rollbackConflictSubtitle(hasDeletion: boolean): string {
  return hasDeletion
    ? "这些文件在 10月12日 升级后又被改动或删除过"
    : "这些文件在 10月12日 升级后又被改动过";
}

export const RESUME_AGAIN = "下次启动还会再提示";
export const CANCEL_FOOTER = "选「取消」什么都不做，下次启动还会再提示。";
export const STOP_BODY = "已停止写入后面的文件，不会自动回滚";
export const STOP_FOOTER = "选「稍后处理」后，下次启动会继续提示这次未完成的升级。历史记录会标成「部分失败」。";

export function resumeExplain(done: number): string {
  return `「继续」会先核对已经写完的 ${done} 个文件有没有被改过，再重新检查失败的文件和剩下的文件，然后从失败的那个文件接着写，用的还是原来的事务和备份。已经写完的文件如果被改过，会按冲突提示你。`;
}

export const RESUME_NOTE = resumeExplain(9);

export const ROLLBACK_RESUME_BODY = "回滚做到一半时被中断了。为了避免已经还原的文件被重新写掉，这里只能接着回滚，不能再继续升级。";

/** 画面 26。计划自己删掉、并且已经从备份恢复的文件不出现在这份名单里。 */
export const PARTIAL_ROLLBACK_RESULT = "部分回滚";
export const PARTIAL_ROLLBACK_CHANGE = "已恢复 7 / 9 · 跳过 2";

export type UnrestoredFile = { path: string; reason: string };

export function partialRollbackIntro(count: number): string {
  return `这 ${count} 个文件没有恢复，保持回滚前的样子：`;
}

export const PARTIAL_ROLLBACK_UNRESTORED: UnrestoredFile[] = [
  { path: "src/routes.js", reason: "升级后被修改，已跳过" },
  { path: "conf/legacy.yml", reason: "被外部删除，已跳过" },
];

export const ATTR_REASON = "内容已恢复，权限 / 修改时间未恢复";

export function attrRestoredNote(count: number): string {
  return `有 ${count} 个文件内容已恢复，权限 / 修改时间未恢复`;
}

export const ATTR_UNRESTORED: UnrestoredFile[] = [
  { path: "bin/deploy.sh", reason: ATTR_REASON },
  { path: "conf/app.yml", reason: ATTR_REASON },
];

/** 复选框文案。目录原样使用外壳传入的 backupDir，不在这里拼路径。 */
export function backupCheckboxLabel(backupDir: string): string {
  return `执行前备份到 ${backupDir}`;
}
