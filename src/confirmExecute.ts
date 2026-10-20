/** 后端按将要执行的计划算出的数量和备份路径。文案只拼这些数字，不再数一遍文件。 */

export type ExecuteConflict = {
  path: string;
  detail: string;
};

export type ExecuteConfirm = {
  execId: string;
  token: string;
  create: number;
  overwrite: number;
  moveCount: number;
  backupPath: string;
  body: string;
  conflicts: ExecuteConflict[];
};

/**
 * 和 core `confirm_execute_body` 同一句。
 * 数量为 0 的类别删掉。移动在场时，它自己带「个文件（保留旧文件）」；否则最后一类带「个文件」。
 */
export function confirmExecuteBody(preview: Pick<ExecuteConfirm, "create" | "overwrite" | "moveCount" | "backupPath">): string {
  const parts: string[] = [];
  if (preview.create > 0) parts.push(`新增 ${preview.create} 个`);
  if (preview.overwrite > 0) parts.push(`覆盖 ${preview.overwrite} 个`);
  if (preview.moveCount > 0) parts.push(`移动 ${preview.moveCount} 个`);
  let lead = "";
  if (parts.length > 0) {
    const last = parts.length - 1;
    if (preview.moveCount > 0) parts[last] = `移动 ${preview.moveCount} 个文件（保留旧文件）`;
    else parts[last] = parts[last].replace(/ 个$/, " 个文件");
    lead = `将${parts.join("、")}。`;
  }
  return `${lead}执行前会把原文件备份到 ${preview.backupPath}，出问题可以一键回滚。`;
}

/** 取消和 Esc 不调用执行。只有「确认执行」才调用。 */
export function settleExecuteConfirm(action: "cancel" | "escape" | "confirm", run: () => void): "cancelled" | "ran" {
  if (action === "confirm") {
    run();
    return "ran";
  }
  return "cancelled";
}
