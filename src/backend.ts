import { invoke } from "@tauri-apps/api/core";
import type { ResumeChoice, RollbackChoice } from "./choices";

export function rollbackRun(execId: string, choice: RollbackChoice) {
  return invoke("rollback_run", { execId, choice });
}

export function resumeRun(execId: string, choice: ResumeChoice) {
  return invoke("resume_run", { execId, choice });
}

export function startupExecId() {
  return invoke<Array<{ execId: string }>>("startup_offers").then((offers) => offers[0]?.execId);
}
