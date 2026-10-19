import { invoke } from "@tauri-apps/api/core";
import type { ResumeChoice, RollbackChoice } from "./choices";
import { parseResume, type ExecReport, type RecoveryOffer, type ResumeView } from "./engine";

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
};

export function listConnections() {
  return invoke<SavedConnection[]>("list_connections");
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

export function remoteWritePrompt(connectionId: string, remotePath: string, content: number[], contentSha256?: string) {
  return invoke<RemoteWritePrompt>("remote_write_prompt", { connectionId, remotePath, content, contentSha256 });
}

export type RemoteWriteOutcome = {
  execId: string;
  remotePath: string;
  backupPath: string | null;
};

export function commitRemoteOverwrite(execId: string) {
  return invoke<RemoteWriteOutcome>("commit_remote_overwrite", { execId });
}

export type JournalRun = {
  execId: string;
  projectRoot?: string | null;
  startedAt: number;
  status: string;
  fileCount: number;
  doneCount: number;
  failedCount: number;
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
