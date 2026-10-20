import { editedContents, shownFilePayload, type LocalPlan } from "./localPlan.ts";

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
  remoteRoot: string | null;
};

/** 先按 POSIX 折叠 `//`、`.` 和 `..`。不是绝对路径，或 `..` 逃出根时返回 null。折叠后没有目录名就是 `/`。 */
function normalizePosixAbsolute(raw: string): string | null {
  const value = raw.trim();
  if (!value.startsWith("/") || value.includes("\\") || value.includes("\0")) return null;
  const segments = posixSegments(value);
  if (!segments) return null;
  if (segments.length === 0) return "/";
  return `/${segments.join("/")}`;
}

/** 规范化之后只剩文件系统根，例如 `/`、`/.`、`//`、`/srv/..`。 */
export function isFilesystemRemoteRoot(root: string): boolean {
  return normalizePosixAbsolute(root) === "/";
}

export function connectionRemoteRoot(connection: { remoteRoot: string | null } | null): string {
  const value = connection?.remoteRoot;
  if (value == null) return "";
  const normalized = normalizePosixAbsolute(value);
  if (normalized == null) return value.trim();
  if (normalized === "/") return "";
  return normalized;
}

/** 绝对 POSIX 目录。规范化成 `/` 的不算。 */
export function isAbsoluteRemoteRoot(root: string): boolean {
  const normalized = normalizePosixAbsolute(root);
  return normalized != null && normalized !== "/";
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

export type RemoteBatchFile = {
  remotePath: string;
  content: number[];
};

export type RemoteFileOutcome = {
  remotePath: string;
  state: string;
  code: string | null;
  /** 写成功且远端原来就有文件。新增文件为 false。 */
  backedUp: boolean;
};

export type RemoteBatchReport = {
  backupDir: string;
  total: number;
  failedIndex: number | null;
  failedPath: string | null;
  code: string | null;
  files: RemoteFileOutcome[];
};

/** 勾选的计划文件全部送去写。打开中的那一个文件不再代表整份计划。 */
export function remotePlanFiles(plan: LocalPlan | null, picked: ReadonlySet<string>): { files: RemoteBatchFile[]; error: string | null } {
  if (!plan) return { files: [], error: null };
  const edited = editedContents();
  const files: RemoteBatchFile[] = [];
  for (const file of plan.files) {
    const rel = file.path.replace(/\\/g, "/");
    if (!picked.has(rel)) continue;
    if (file.kind === "delete") return { files: [], error: `远程写入还不能删除：${rel}` };
    const remotePath = joinRemote(remoteRoot, rel);
    if (!remotePath) return { files: [], error: `远程路径越出了远程目录：${rel}` };
    const content = Object.prototype.hasOwnProperty.call(edited, file.path)
      ? Array.from(new TextEncoder().encode(edited[file.path]))
      : file.bytes
        ? file.bytes.slice()
        : null;
    if (!content) return { files: [], error: `远程写入还没有读到 ${rel} 的内容` };
    files.push({ remotePath, content });
  }
  return { files, error: null };
}

export function remoteFileMark(file: { state: string; code?: string | null }): string {
  if (file.state === "written") return "已写入";
  if (file.state === "pending") return "未开始";
  return `失败 · ${file.code || "E_IO"}`;
}

export function remoteBatchFailure(report: RemoteBatchReport): string | null {
  if (report.failedIndex == null || !report.failedPath) return null;
  const n = report.failedIndex;
  const written = report.files.filter((file) => file.state === "written");
  const backed = written.filter((file) => file.backedUp).length;
  const created = written.length - backed;
  let text = `写到第 ${n}/共 ${report.total} 个文件时失败：${report.failedPath}。`;
  if (backed > 0) {
    text += `已写入的 ${backed} 个文件的原文件已备份到本机（${report.backupDir}），可以从这里手工恢复。`;
  }
  if (created > 0) {
    text += `新增的 ${created} 个文件原本不存在，如需撤销请删除。`;
  }
  return text;
}

/** 只有「仍然写入」会调用 write。取消什么都不写。 */
export function settleRemoteWrite(decision: string, write: () => void): "cancelled" | "wrote" {
  if (decision !== PROCEED_LABEL) {
    return "cancelled";
  }
  write();
  return "wrote";
}
