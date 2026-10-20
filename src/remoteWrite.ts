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

/** 选中连接的 remoteRoot。空字符串表示还没填，或这版连接对象上还没有该字段。 */
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
  remoteRoot?: string | null;
};

export function connectionRemoteRoot(connection: { remoteRoot?: string | null } | null): string {
  const value = connection?.remoteRoot;
  return typeof value === "string" ? value : "";
}

/** 绝对 POSIX 目录。含 `..` 且逃出根、或不是以 / 开头，都不算。 */
export function isAbsoluteRemoteRoot(root: string): boolean {
  if (!root.startsWith("/") || root.includes("\\") || root.includes("\0") || root.split("/").includes("..")) return false;
  return posixSegments(root) != null;
}

export function noteRemoteRoot(root: string) {
  remoteRoot = root;
}

function posixSegments(path: string): string[] | null {
  if (path.includes("\\") || path.includes("\0")) return null;
  const out: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (out.length === 0) return null;
      out.pop();
      continue;
    }
    out.push(part);
  }
  return out;
}

/** 把远程目录和计划相对路径按 POSIX 拼起来。逃出根、绝对相对路径、或根不是绝对路径时返回空字符串。 */
export function joinRemote(root: string, rel: string): string {
  if (!isAbsoluteRemoteRoot(root)) return "";
  const relPosix = rel.replace(/\\/g, "/");
  if (!relPosix || relPosix.startsWith("/") || relPosix.includes("\0")) return "";
  const rootSeg = posixSegments(root);
  if (!rootSeg) return "";
  const stack = [...rootSeg];
  let added = 0;
  for (const part of relPosix.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (added === 0) return "";
      stack.pop();
      added -= 1;
      continue;
    }
    stack.push(part);
    added += 1;
  }
  if (added <= 0) return "";
  for (let i = 0; i < rootSeg.length; i += 1) {
    if (stack[i] !== rootSeg[i]) return "";
  }
  return `/${stack.join("/")}`;
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
