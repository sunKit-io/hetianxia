import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ResumeChoice, RollbackChoice } from "./choices";
import { parseResume, type ExecReport, type RecoveryOffer, type ResumeView } from "./engine";
import type { ExecuteConfirm } from "./confirmExecute";
import type { ExecPlan, SideText, TextDoc } from "./localPlan";
import type { RemoteBatchFile, RemoteBatchReport } from "./remoteWrite";

export function rollbackRun(execId: string, choice: RollbackChoice) {
  return invoke<ExecReport>("rollback_run", { execId, choice });
}

export function resumeRun(execId: string, choice: ResumeChoice): Promise<ResumeView> {
  return invoke("resume_run", { execId, choice }).then(parseResume);
}

export function startupOffers() {
  return invoke<RecoveryOffer[]>("startup_offers");
}

export function startupFailure() {
  return invoke<string | null>("startup_failure");
}

export type RemoteWritePrompt = {
  title: string;
  body: string;
  backupDir: string;
  cancelLabel: string;
  proceedLabel: string;
  defaultButton: string;
  execId: string;
};

export function appBakDir() {
  return invoke<string>("app_bak_dir");
}

export type SavedConnection = {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  auth: "password" | "key" | "agent";
  keyPath: string;
  fingerprint: string;
  /** 远端绝对目录。旧数据没有这一列时是 null。 */
  remoteRoot: string | null;
};

export type ImportedHost = {
  alias: string;
  hostName: string;
  user: string | null;
  port: number;
  proxyJump: string | null;
  identityFile: string;
};

export function listConnections() {
  return invoke<SavedConnection[]>("list_connections");
}

export function readSshConfig() {
  return invoke<ImportedHost[]>("read_ssh_config");
}

export function saveConnection(conn: SavedConnection) {
  return invoke<SavedConnection>("save_connection", { conn });
}

export function deleteConnection(id: string) {
  return invoke<void>("delete_connection", { id });
}

export function clearConnectionFingerprint(id: string) {
  return invoke<void>("clear_connection_fingerprint", { id });
}

export type ProbeReport = {
  fingerprint: string;
  hostKey: string;
  authOk: boolean;
  sftpOk: boolean;
  sha256sumOk: boolean;
};

export function testConnection(id: string, acceptNew: boolean, confirmedFingerprint?: string) {
  return invoke<ProbeReport>("test_connection", { id, acceptNew, confirmedFingerprint });
}

export function saveSshSecret(account: string, secret: string) {
  return invoke<{ saved: boolean }>("save_ssh_secret", { account, secret });
}

export function sshSecretStatus(account: string) {
  return invoke<{ saved: boolean }>("ssh_secret_status", { account });
}

export function deleteSshSecret(account: string) {
  return invoke<void>("delete_ssh_secret", { account });
}

export function remoteWritePrompt(connectionId: string, files: RemoteBatchFile[]) {
  return invoke<RemoteWritePrompt>("remote_write_prompt", { connectionId, files });
}

export function commitRemoteOverwrite(execId: string) {
  return invoke<RemoteBatchReport>("commit_remote_overwrite", { execId });
}

export function listRemoteDirs(connectionId: string, path: string) {
  return invoke<{ path: string; dirs: string[] }>("list_remote_dirs", { connectionId, path });
}

export type JournalRun = {
  execId: string;
  projectRoot?: string | null;
  startedAt: number;
  status: string;
  fileCount: number;
  doneCount: number;
  failedCount: number;
  conflictCount: number;
  remote: boolean;
};

export function journalRuns() {
  return invoke<JournalRun[]>("journal_runs");
}

export type RollbackPreview = {
  startedAt: number;
  files: { path: string; detail: string }[];
};

export function rollbackPreview(execId: string) {
  return invoke<RollbackPreview>("rollback_preview", { execId });
}

export type ScanFile = {
  rel: string;
  status: string;
  movedFrom?: string | null;
  movedTo?: string | null;
  message?: string | null;
  readonly?: boolean;
  leftSize?: number | null;
  rightSize?: number | null;
  leftMtimeMs?: number | null;
  rightMtimeMs?: number | null;
  contentSha256?: string | null;
  entryName?: string | null;
  leftSha256?: string | null;
};

export function scanPair(left: string, right: string) {
  return invoke<{ files: ScanFile[]; scanId: string; packageKind: string }>("scan", { left, right });
}

export type FileDiff = {
  left: TextDoc;
  right: TextDoc;
};

export function diffTexts(left: string, right: string) {
  return invoke<FileDiff>("diff", { left, right });
}

export function mergeTexts(left: string, right: string, decision: string) {
  return invoke<{ text: string }>("merge", { left, right, decision });
}

export function readSide(root: string, rel: string) {
  return invoke<SideText>("read_side", { root, rel });
}

export function preparePackage(path: string) {
  return invoke<string>("prepare_package", { path });
}

export function confirmExecute(root: string, plan: ExecPlan, allowReadonly: boolean) {
  return invoke<ExecuteConfirm>("confirm_execute", { root, plan, allowReadonly });
}

export function executeLocal(root: string, plan: ExecPlan, allowReadonly: boolean, token: string) {
  return invoke<ExecReport>("execute", { root, plan, allowReadonly, token });
}

export async function pickProjectDirectory(): Promise<string | null> {
  const picked = await open({ directory: true, multiple: false, title: "选择项目目录" });
  return typeof picked === "string" ? picked : null;
}

export type UpgradePackage = { source: string; prepared: string };

/** scan 的右边是目录。zip 先解开；文件夹原样返回。两条入口都进这里。source 是用户选中的路径，用来记住勾选。 */
async function useUpgradePackage(picked: string | string[] | null): Promise<UpgradePackage | null> {
  if (typeof picked !== "string") return null;
  return { source: picked, prepared: await preparePackage(picked) };
}

/** 只选文件夹。和 zip 入口解开后走同一条 scan。 */
export function pickUpgradeDirectory(): Promise<UpgradePackage | null> {
  return open({ directory: true, multiple: false, title: "选择升级包文件夹" }).then(useUpgradePackage);
}

/** 只选 zip。和文件夹入口解开后走同一条 scan。 */
export function pickUpgradeZip(): Promise<UpgradePackage | null> {
  return open({
    multiple: false,
    title: "选择 zip 升级包",
    filters: [{ name: "升级包", extensions: ["zip"] }],
  }).then(useUpgradePackage);
}

export function loadFilePicks(projectRoot: string, packagePath: string) {
  return invoke<string[]>("load_file_picks", { projectRoot, packagePath });
}

export function saveFilePicks(projectRoot: string, packagePath: string, skipped: string[]) {
  return invoke<void>("save_file_picks", { projectRoot, packagePath, skipped });
}
