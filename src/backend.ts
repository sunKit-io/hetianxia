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

export function remoteWritePrompt(host: string, port: number, remotePath: string) {
  return invoke<RemoteWritePrompt>("remote_write_prompt", { host, port, remotePath });
}

export type RemoteWriteOutcome = {
  execId: string;
  remotePath: string;
  backupPath: string | null;
};

export function commitRemoteOverwrite(args: {
  execId: string;
  host: string;
  port: number;
  user: string;
  remotePath: string;
  bytes: number[];
  auth: string;
  account: string;
  keyPath: string;
}) {
  return invoke<RemoteWriteOutcome>("commit_remote_overwrite", { req: args });
}
