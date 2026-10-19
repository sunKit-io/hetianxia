import type { ResumeChoice } from "./choices";

/** 和核心 ExecReport / ResumeOutcome 的 camelCase 序列化一致。 */

export type FileResult = {
  index: number;
  path: string;
  kind: string;
  state: string;
  error?: string | null;
  code?: string | null;
};

export type ExecReport = {
  execId: string;
  state: string;
  files: FileResult[];
  backupRoot: string;
  logStopped: boolean;
  historyResult: string;
  sheetActions: string[];
};

export type ConflictFile = { path: string; detail: string };

export type ChangeHit = { path: string; hashChanged: boolean; detail: string };

export type RecoveryOffer = {
  execId: string;
  state: string;
  preRecoveryState: string;
  actions: string[];
  conflicts: ConflictFile[];
  code?: string | null;
  error?: string | null;
};

export type ResumeView =
  | { kind: "unchanged" }
  | { kind: "continued"; report: ExecReport }
  | { kind: "rollbackPrompt"; title: string; files: ConflictFile[] }
  | { kind: "changesSinceScan"; hits: ChangeHit[] };

export type NextScreen =
  | { sheet: null; status: string | null }
  | { sheet: "external"; title: string; files: ConflictFile[]; status: null }
  | { sheet: "pending"; files: ConflictFile[]; status: null }
  | { sheet: "rollback"; files: ConflictFile[]; status: null };

const SPEC: Record<string, string> = {
  NOT_FOUND: "E_NOT_FOUND",
  PERMISSION: "E_PERMISSION",
  BUSY: "E_BUSY",
  HOSTKEY_CHANGED: "E_HOSTKEY_CHANGED",
  SFTP_UNAVAILABLE: "E_SFTP_UNAVAILABLE",
  AUTH: "E_AUTH",
  NET: "E_NET",
  CHANGED_SINCE_SCAN: "E_CHANGED_SINCE_SCAN",
  CASE_CONFLICT: "E_CASE_CONFLICT",
  NO_SPACE: "E_NO_SPACE",
  ZIP_SLIP: "E_ZIP_SLIP",
  ZIP_BOMB: "E_ZIP_BOMB",
  ZIP_ENCODING: "E_ZIP_ENCODING",
  CANCELLED: "E_CANCELLED",
  SYMLINK_IN_PATH: "E_SYMLINK_IN_PATH",
  SYMLINK_REFUSED: "E_SYMLINK_REFUSED",
  UNCONFIRMED_HUNKS: "E_UNCONFIRMED_HUNKS",
  INVALID_LOCATION: "E_INVALID_LOCATION",
  FAULT: "E_FAULT",
  LOG_WRITE: "E_LOG_WRITE",
  VERIFY_FAILED: "E_VERIFY_FAILED",
  REMOTE_HASH_UNAVAILABLE: "E_REMOTE_HASH_UNAVAILABLE",
  BACKUP_PURGED: "E_BACKUP_PURGED",
  ROLLBACK_CONFLICT: "E_ROLLBACK_CONFLICT",
  ROLLBACK_PARTIAL: "E_ROLLBACK_PARTIAL",
  CONFLICT: "E_CONFLICT",
  UNSUPPORTED: "E_UNSUPPORTED",
  IO: "E_IO",
  INVALID_STATE: "E_INVALID_STATE",
  INTERNAL: "E_INTERNAL",
};

/** serde 的 SCREAMING_SNAKE_CASE 补上 E_ 前缀。已经是 E_IO 这种形式的原样返回。 */
export function engineCode(code: string): string {
  if (code.startsWith("E_")) return code;
  return SPEC[code] ?? `E_${code}`;
}

export function formatEngineError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object") {
    const rec = err as Record<string, unknown>;
    const code = rec.code != null ? engineCode(String(rec.code)) : "";
    const message = rec.message != null ? String(rec.message) : "";
    if (code && message) return `${code} · ${message}`;
    if (code) return code;
    if (message) return message;
  }
  if (err instanceof Error) return err.message;
  return String(err);
}

const FILE_STATE: Record<string, string> = {
  done: "已写入",
  skipped: "已跳过",
  failed: "失败",
  doing: "进行中",
  pending: "未开始",
  conflict: "有冲突",
};

/** 执行结果列表：状态用中文，错误码原样保留。 */
export function formatFileResult(file: Pick<FileResult, "state" | "code" | "error">): string {
  const state = FILE_STATE[file.state] ?? file.state;
  return [state, file.code, file.error].filter(Boolean).join(" · ");
}

export function formatReport(report: ExecReport): string {
  const head = report.historyResult ? `${report.state} · ${report.historyResult}` : report.state;
  const lines = [head];
  for (const file of report.files) {
    if (!file.code && !file.error) continue;
    const bits = [file.path];
    if (file.code) bits.push(engineCode(file.code));
    if (file.error) bits.push(file.error);
    lines.push(bits.join(" · "));
  }
  return lines.join("\n");
}

export function formatOffer(offer: RecoveryOffer): string {
  return [offer.state, offer.code ? engineCode(offer.code) : "", offer.error ?? ""].filter(Boolean).join(" · ");
}

/** 外部标记枚举：单元变体是字符串，其余是单键对象。 */
export function parseResume(raw: unknown): ResumeView {
  if (raw === "unchanged") return { kind: "unchanged" };
  if (raw && typeof raw === "object") {
    const rec = raw as Record<string, unknown>;
    if ("unchanged" in rec) return { kind: "unchanged" };
    if (rec.continued && typeof rec.continued === "object") {
      return { kind: "continued", report: rec.continued as ExecReport };
    }
    if (rec.rollbackPrompt && typeof rec.rollbackPrompt === "object") {
      const prompt = rec.rollbackPrompt as { title?: string; files?: ConflictFile[] };
      return { kind: "rollbackPrompt", title: prompt.title ?? "", files: prompt.files ?? [] };
    }
    if (Array.isArray(rec.changesSinceScan)) {
      return { kind: "changesSinceScan", hits: rec.changesSinceScan as ChangeHit[] };
    }
  }
  throw new Error("E_INTERNAL · 无法识别继续的结果");
}

/** 继续的下一步由引擎返回值决定，不按按钮预先翻页。 */
export function afterResume(choice: ResumeChoice, outcome: ResumeView): NextScreen {
  if (outcome.kind === "unchanged") return { sheet: null, status: null };
  if (outcome.kind === "continued") return { sheet: null, status: formatReport(outcome.report) };
  if (outcome.kind === "changesSinceScan") {
    return {
      sheet: "pending",
      files: outcome.hits.map((hit) => ({ path: hit.path, detail: hit.detail })),
      status: null,
    };
  }
  if (choice === "rollback") return { sheet: "rollback", files: outcome.files, status: null };
  return { sheet: "external", title: outcome.title, files: outcome.files, status: null };
}
