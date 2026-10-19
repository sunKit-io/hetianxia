/** 历史行按钮只带这一行的 exec_id。回滚先打开选择，不直接调用 rollback_run。 */

export const REMOTE_ROLLBACK_TITLE = "远程写入暂不支持回滚";

export type HistoryContinue = {
  command: "resume_run";
  execId: string;
  choice: "continue";
};

export type HistoryStatusLabel = "已完成" | "部分失败" | "未完成" | "已回滚" | "部分回滚" | "未知状态";

/** 设计 19。日志里的原始状态只在这里换成界面文案。 */
export function historyStatusLabel(status: string): HistoryStatusLabel {
  switch (status) {
    case "COMMITTED":
      return "已完成";
    case "STOPPED":
      return "部分失败";
    case "PREPARED":
    case "BACKED_UP":
    case "VERIFYING":
    case "LOG_FAILED":
      return "未完成";
    case "ROLLED_BACK":
      return "已回滚";
    case "ROLLBACK_PARTIAL":
      return "部分回滚";
    default:
      return "未知状态";
  }
}

/** 继续只给未完成和部分失败。已回滚没有按钮。已完成只在回滚仍有效时给回滚。 */
export function historyRowButtons(label: HistoryStatusLabel): { rollback: boolean; resume: boolean } {
  if (label === "未完成" || label === "部分失败") return { rollback: true, resume: true };
  if (label === "已完成" || label === "部分回滚") return { rollback: true, resume: false };
  return { rollback: false, resume: false };
}

export function historyContinue(execId: string): HistoryContinue {
  return { command: "resume_run", execId, choice: "continue" };
}

export function formatRunTime(startedAt: number): string {
  if (!Number.isFinite(startedAt) || startedAt <= 0) return "";
  const date = new Date(startedAt);
  const month = date.getMonth() + 1;
  const day = date.getDate();
  const hour = String(date.getHours()).padStart(2, "0");
  const minute = String(date.getMinutes()).padStart(2, "0");
  return `${month}月${day}日 ${hour}:${minute}`;
}

export function formatFileCounts(run: { fileCount: number; doneCount: number; failedCount: number }): string {
  return `共 ${run.fileCount} · 已写 ${run.doneCount} · 失败 ${run.failedCount}`;
}
