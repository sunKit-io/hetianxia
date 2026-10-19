/** 设计 21 的行标签和标题。和核心里的 DETAIL_MODIFIED / resume_conflict_title 同一套文案。 */

export const DETAIL_MODIFIED = "升级后被修改";
export const DETAIL_DELETED = "升级后被删除";

export type ResumeFile = { path: string; detail: string };

export function resumeConflictTitle(files: ResumeFile[]): string {
  const n = files.length;
  if (files.some((file) => file.detail === DETAIL_DELETED)) {
    return `有 ${n} 个已经写完的文件被改过或删除`;
  }
  return `有 ${n} 个已经写完的文件被改过`;
}

/** 有删除走画面 23，否则走画面 21。 */
export function resumeConflictScreen(files: ResumeFile[]): "21" | "23" {
  return files.some((file) => file.detail === DETAIL_DELETED) ? "23" : "21";
}

export function pendingChangedTitle(count: number): string {
  return `还有 ${count} 个还没写的文件被改过`;
}

export const SKIP_PENDING = "跳过这些文件，继续其余的";
export const OVERWRITE_PENDING = "仍然覆盖";
export const OVERWRITE_PENDING_NOTE = "先重新备份并校验你现在的版本，通过之后才覆盖";

export function rollbackProceedLabel(hasDeletion: boolean): string {
  return hasDeletion ? "继续回滚" : "继续回滚（先备份当前版本）";
}

export function rollbackProceedNote(hasDeletion: boolean): string {
  return hasDeletion
    ? "从备份恢复，删除的文件会重新出现"
    : "先把这些文件的当前版本备份，再覆盖回升级前的样子";
}

export function rollbackSkipNote(hasDeletion: boolean): string {
  return hasDeletion ? "保持现状。已经删除的文件不会恢复" : "其余文件照常回滚，这些保持现状";
}

export const RESUME_AGAIN = "下次启动还会再提示";
export const STOP_BODY = "已停止写入后面的文件，不会自动回滚";
export const STOP_FOOTER = "下次启动还会再提示。历史记录会标成「部分失败」。";
export const RESUME_NOTE = "继续会先核对已写完的文件，沿用原来的事务和备份";

/** 画面 26。计划自己删掉、并且已经从备份恢复的文件不出现在这份名单里。 */
export const PARTIAL_ROLLBACK_RESULT = "部分回滚";
export const PARTIAL_ROLLBACK_CHANGE = "已恢复 7 / 9 · 跳过 2";

export type UnrestoredFile = { path: string; reason: string };

export const PARTIAL_ROLLBACK_UNRESTORED: UnrestoredFile[] = [
  { path: "src/routes.js", reason: "升级后被修改，已跳过" },
  { path: "public/index.html", reason: "升级后被修改，已跳过" },
];
