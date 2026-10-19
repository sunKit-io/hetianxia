import { shownFilePayload } from "./localPlan.ts";

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
};

/** 预演选中的计划文件。目标路径和要写入的新内容都来自这一份。 */
export type RemotePlanFile = {
  remotePath: string;
  content: string;
};

/**
 * fd82620 的 SavedConnection 没有远程目录。keyPath 是本机私钥。
 * 写入许可上的 remote_path 由「连接远程根 + 计划文件相对路径」拼成；根为空时路径就是相对路径。
 */
let remoteRoot = "";

export type RemoteConnection = {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  auth: string;
  keyPath: string;
  fingerprint: string;
};

export function connectionRemoteRoot(connection: RemoteConnection | null): string {
  if (!connection) return "";
  return "";
}

export function noteRemoteRoot(root: string) {
  remoteRoot = root;
}

export function joinRemote(root: string, rel: string): string {
  const file = rel.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!file || file.split("/").some((part) => part === "" || part === "." || part === "..")) return "";
  const base = root.replace(/\\/g, "/").replace(/\/+$/, "");
  if (!base) return file;
  if (base.split("/").some((part) => part === "." || part === "..")) return "";
  return `${base}/${file}`;
}

export function currentRemotePlan(): RemotePlanFile | null {
  const file = shownFilePayload();
  if (!file) return null;
  const remotePath = joinRemote(remoteRoot, file.path);
  if (!remotePath) return null;
  return { remotePath, content: file.content };
}

/** 只有「仍然写入」会调用 write。取消什么都不写。 */
export function settleRemoteWrite(decision: string, write: () => void): "cancelled" | "wrote" {
  if (decision !== PROCEED_LABEL) {
    return "cancelled";
  }
  write();
  return "wrote";
}
