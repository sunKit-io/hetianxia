export const BUNDLE_ID = "com.hetianxia.app";
export const DIALOG_TITLE = "远程写入暂不支持回滚";
export const CANCEL_LABEL = "取消";
export const PROCEED_LABEL = "仍然写入";

declare const __HETIANXIA_APP_DATA_DIR__: string;

/** Tauri `app_data_dir()`：数据目录再拼上 bundle id。 */
export function appDataDir(home: string, os: "macos" | "linux" | "windows", xdgDataHome?: string): string {
  if (os !== "windows" && !home.startsWith("/")) {
    throw new Error("主目录必须是绝对路径");
  }
  const root = home.replace(/[\\/]+$/, "");
  if (os === "macos") return `${root}/Library/Application Support/${BUNDLE_ID}`;
  if (os === "windows") return `${root}\\AppData\\Roaming\\${BUNDLE_ID}`;
  const base = xdgDataHome && xdgDataHome.startsWith("/") ? xdgDataHome.replace(/\/+$/, "") : `${root}/.local/share`;
  return `${base}/${BUNDLE_ID}`;
}

/** `app_data_dir()/bak/<id>`，和本地引擎的 backup_root/<id> 相同。 */
export function txBackupDir(appDataDir: string, execId: string): string {
  if (!execId || execId.includes("/") || execId.includes("\\") || execId.includes("..") || execId.includes("\0")) {
    throw new Error("备份编号不合法");
  }
  if (!appDataDir.startsWith("/") && !/^[A-Za-z]:\\/.test(appDataDir)) {
    throw new Error("应用数据目录必须是绝对路径");
  }
  const sep = appDataDir.includes("\\") && !appDataDir.includes("/") ? "\\" : "/";
  return `${appDataDir.replace(/[\\/]+$/, "")}${sep}bak${sep}${execId}`;
}

export function remoteWriteBody(backupDir: string): string {
  return `写入前会把远端原文件备份到本机（${backupDir}），出问题可以从这里手工恢复。`;
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

export function beginRemoteWrite(appData: string, execId: string): PendingRemoteWrite {
  const backupDir = txBackupDir(appData, execId);
  return {
    execId,
    backupDir,
    title: DIALOG_TITLE,
    body: remoteWriteBody(backupDir),
    cancelLabel: CANCEL_LABEL,
    proceedLabel: PROCEED_LABEL,
    defaultButton: CANCEL_LABEL,
  };
}

export function newExecId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** 只有「仍然写入」会调用 write。取消什么都不写。 */
export function settleRemoteWrite(decision: string, write: () => void): "cancelled" | "wrote" {
  if (decision !== PROCEED_LABEL) {
    return "cancelled";
  }
  write();
  return "wrote";
}

export function previewAppDataDir(): string {
  if (typeof __HETIANXIA_APP_DATA_DIR__ !== "string" || __HETIANXIA_APP_DATA_DIR__.length === 0) {
    throw new Error("没有应用数据目录，不能确定备份路径");
  }
  return __HETIANXIA_APP_DATA_DIR__;
}
