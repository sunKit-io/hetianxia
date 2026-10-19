export const DIALOG_TITLE = "远程写入暂不支持回滚";
export const CANCEL_LABEL = "取消";
export const PROCEED_LABEL = "仍然写入";
export const VERIFY_FAILED_TITLE = "远端文件写完后核对不一致";
export const VERIFY_FAILED_BUTTON = "好";
export const VERIFY_FAILED_CODE = "E_VERIFY_FAILED";

export function remoteWriteBody(backupDir: string): string {
  return `写入前会把远端原文件备份到本机（${backupDir}），出问题可以从这里手工恢复。`;
}

export function verifyFailureBody(remotePath: string, backupPath: string): string {
  return `${remotePath} 写入后的内容和预期不一致。写入前的原文件已备份到本机（${backupPath}），可以从这里手工恢复。`;
}

export type PendingRemoteWrite = {
  execId: string;
  backupDir: string;
  title: string;
  body: string;
  cancelLabel: string;
  proceedLabel: string;
  defaultButton: string;
  host: string;
  port: number;
  user: string;
  account: string;
  keyPath: string;
  remotePath: string;
};

/** 只有「仍然写入」会调用 write。取消什么都不写。 */
export function settleRemoteWrite(decision: string, write: () => void): "cancelled" | "wrote" {
  if (decision !== PROCEED_LABEL) {
    return "cancelled";
  }
  write();
  return "wrote";
}
