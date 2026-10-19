/** 历史行按钮只带这一行的 exec_id。不读启动提示上的那一次。 */

export const REMOTE_ROLLBACK_TITLE = "远程写入暂不支持回滚";

export type HistoryRollback = {
  command: "rollback_run";
  execId: string;
  choice: "proceedBackupCurrent";
};

export type HistoryContinue = {
  command: "resume_run";
  execId: string;
  choice: "continue";
};

export type HistoryAction = HistoryRollback | HistoryContinue;

export function historyRollback(execId: string): HistoryRollback {
  return { command: "rollback_run", execId, choice: "proceedBackupCurrent" };
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
