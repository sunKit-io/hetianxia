/** 界面上的选择对应后端枚举。密码和远程写入不在这里。 */

export type RollbackChoice = "cancel" | "skipConflicts" | "proceedBackupCurrent";
export type ResumeChoice = "cancel" | "continue" | "keepExternal" | "rollback" | "skipChanged" | "overwriteChanged";

/** 「备份当前」是 proceedBackupCurrent，「跳过」是 skipConflicts。 */
export function rollbackChoice(ui: "proceed" | "skip" | "cancel"): RollbackChoice {
  if (ui === "skip") return "skipConflicts";
  if (ui === "cancel") return "cancel";
  return "proceedBackupCurrent";
}

/** 「保留外部修改并继续」是 keepExternal。选回滚则进入回滚流程，这里不写文件。 */
export function externalChoice(ui: "keep" | "rollback"): ResumeChoice {
  return ui === "keep" ? "keepExternal" : "rollback";
}

/** 还没写入的文件：跳过或仍然覆盖。 */
export function pendingChoice(ui: "skip" | "overwrite"): ResumeChoice {
  return ui === "overwrite" ? "overwriteChanged" : "skipChanged";
}

/** 「移动（保留旧文件）」把 keep_source 交给执行计划。普通移动不保留。 */
export function keepSource(label: string): boolean {
  return label.includes("保留旧文件");
}
