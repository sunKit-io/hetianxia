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
