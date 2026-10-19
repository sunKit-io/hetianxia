import { useState } from "react";
import { AlertTriangle, Clock, ShieldCheck, XCircle } from "lucide-react";
import { keepSource } from "./choices";
import {
  PENDING_FILES,
  PREVIEW_NOTE,
  PREVIEW_ROWS,
  PREVIEW_STATS,
  RESUME_FILE,
  ROLLBACK_PROGRESS,
  SCAN_LINE,
  SCAN_TITLE,
  STOPPED,
  type ChangeRow,
} from "./demoData";
import {
  CANCEL_FOOTER,
  OVERWRITE_PENDING,
  OVERWRITE_PENDING_NOTE,
  RESUME_NOTE,
  ROLLBACK_CHOICE_NOTE,
  ROLLBACK_RESUME_BODY,
  SKIP_PENDING,
  STOP_BODY,
  STOP_FOOTER,
  backupCheckboxLabel,
  isDeletedDetail,
  keepExternalNote,
  pendingChangedTitle,
  resumeConflictTitle,
  rollbackConflictSubtitle,
  rollbackProceedLabel,
  rollbackProceedNote,
  rollbackSkipNote,
  skipPendingNote,
  type ResumeFile,
} from "./resumeCopy";

function FileList({ files }: { files: ResumeFile[] }) {
  return (
    <ul className="filelist">
      {files.map((file) => {
        const deleted = isDeletedDetail(file.detail, file.deleted);
        return (
          <li key={file.path}>
            <span className={deleted ? "strike" : ""}>{file.path}</span>
            <em className={deleted ? "bad" : ""}>{file.detail}</em>
          </li>
        );
      })}
    </ul>
  );
}

export function PreviewSheet({
  backupDir,
  onCancel,
  onRun,
}: {
  backupDir: string;
  onCancel: () => void;
  onRun: () => void;
}) {
  const [scan, setScan] = useState<"pending" | "rescan" | "skip" | "overwrite">("pending");
  const [backupOn, setBackupOn] = useState(true);
  const scanLabel = scan === "rescan" ? "已重新扫描" : scan === "skip" ? "跳过" : scan === "overwrite" ? "仍然覆盖" : "";
  return (
    <div className="mask" data-screen="03" onClick={onCancel}>
      <div className="sheet wide" onClick={(e) => e.stopPropagation()}>
        <h1><ShieldCheck size={22} /> 确认执行升级</h1>
        <p className="sub">OrderCenter · 本地 ~/work/order-center ← release-3.0.0.zip</p>
        <div className="stats previewstats">
          {PREVIEW_STATS.map((item) => (
            <b key={item.label}>
              {item.n}
              <small>{item.label}{item.sub ? <span> {item.sub}</span> : null}</small>
            </b>
          ))}
        </div>
        <div className="scanbox">
          <AlertTriangle size={16} />
          <div>
            <strong>{SCAN_TITLE}</strong>
            <p>{SCAN_LINE}{scanLabel ? ` · ${scanLabel}` : ""}</p>
            <div className="scanacts">
              <button type="button" onClick={() => setScan("rescan")}>重新扫描该文件</button>
              <button type="button" onClick={() => setScan("skip")}>跳过</button>
              <button type="button" onClick={() => setScan("overwrite")}>仍然覆盖</button>
            </div>
          </div>
        </div>
        <p className="note">{PREVIEW_NOTE}</p>
        <div className="listhead">
          <h2>变更清单</h2>
          <button type="button" className="linkish" onClick={() => exportList(PREVIEW_ROWS)}>导出清单</button>
        </div>
        <ul className="changelist">
          {PREVIEW_ROWS.map((row) => (
            <li key={row.path} data-keep-source={keepSource(row.keepOld ? "移动（保留旧文件）" : "移动") ? "1" : "0"}>
              <i className={row.kind}>{row.kind}</i>
              <span>{row.path}</span>
              <em>{row.source}</em>
            </li>
          ))}
        </ul>
        <label className="bak">
          <input type="checkbox" checked={backupOn} onChange={(e) => setBackupOn(e.target.checked)} />
          {backupCheckboxLabel(backupDir)}
        </label>
        <footer>
          <button type="button" className="ghost" onClick={onCancel}>取消</button>
          <button type="button" className="primary" onClick={onRun} disabled={!backupOn}>备份并执行</button>
        </footer>
      </div>
    </div>
  );
}

function exportList(rows: ChangeRow[]) {
  const text = rows.map((row) => `${row.kind}\t${row.path}\t${row.source}`).join("\n");
  const blob = new Blob([text], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "变更清单.txt";
  a.click();
  URL.revokeObjectURL(url);
}

export function WriteFailureSheet({ onLater, onRollback }: { onLater: () => void; onRollback: () => void }) {
  return (
    <div className="mask" data-screen="18">
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="danger"><XCircle size={22} /> 升级已停止：有文件写入失败</h1>
        <p className="sub">{STOP_BODY}</p>
        <div className="infocard">
          <div>
            <p>已完成 {STOPPED.done} / {STOPPED.total} 个文件，失败 {STOPPED.failed} 个</p>
            <p className="path">{STOPPED.file}</p>
            <p className="err">{STOPPED.code} · {STOPPED.message}</p>
          </div>
          <p className="sidetext">{STOPPED.manifest}</p>
        </div>
        <p className="foot">{STOP_FOOTER}</p>
        <footer>
          <button type="button" className="ghost" onClick={onLater}>稍后处理</button>
          <button type="button" className="primary" onClick={onRollback}>回滚</button>
        </footer>
      </div>
    </div>
  );
}

export function StartupSheet({
  onLater,
  onRollback,
  onContinue,
}: {
  onLater: () => void;
  onRollback: () => void;
  onContinue: () => void;
}) {
  return (
    <div className="mask" data-screen="20">
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="warn"><Clock size={22} /> 有一次升级没有完成</h1>
        <p className="sub">{STOPPED.when} · {STOPPED.where}</p>
        <div className="infocard">
          <div>
            <p>已写 {STOPPED.done} / {STOPPED.total} 个文件，失败 {STOPPED.failed} 个</p>
            <p className="path">{STOPPED.file}</p>
            <p className="err">{STOPPED.code} · {STOPPED.message}</p>
          </div>
          <p className="sidetext">{STOPPED.side}</p>
        </div>
        <p className="explain">{RESUME_NOTE}</p>
        <footer>
          <button type="button" className="ghost" onClick={onLater}>稍后处理</button>
          <button type="button" className="ghost" onClick={onRollback}>回滚</button>
          <button type="button" className="primary" onClick={onContinue}>继续</button>
        </footer>
      </div>
    </div>
  );
}

export function ExternalSheet({
  files,
  onCancel,
  onKeep,
  onRollback,
}: {
  files: ResumeFile[];
  onCancel: () => void;
  onKeep: () => void;
  onRollback: () => void;
}) {
  const [choice, setChoice] = useState<"keep" | "rollback">("keep");
  const deleted = files.some((file) => isDeletedDetail(file.detail, file.deleted));
  const screen = deleted ? "23" : "21";
  return (
    <div className="mask" data-screen={screen} onClick={onCancel}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="warn"><XCircle size={22} /> {resumeConflictTitle(files)}</h1>
        <p className="sub">继续之前先确认怎么处理这些文件</p>
        <FileList files={files} />
        <label className={choice === "keep" ? "choice on" : "choice"}>
          <input type="radio" name={`external-${screen}`} checked={choice === "keep"} onChange={() => setChoice("keep")} />
          <span>保留外部修改并继续<small>{keepExternalNote(files.length, deleted, RESUME_FILE)}</small></span>
        </label>
        <label className={choice === "rollback" ? "choice on" : "choice"}>
          <input type="radio" name={`external-${screen}`} checked={choice === "rollback"} onChange={() => setChoice("rollback")} />
          <span>回滚<small>{ROLLBACK_CHOICE_NOTE}</small></span>
        </label>
        <p className="foot">{CANCEL_FOOTER}</p>
        <footer>
          <button type="button" className="ghost" onClick={onCancel}>取消</button>
          <button type="button" className="primary" onClick={() => (choice === "keep" ? onKeep() : onRollback())}>继续</button>
        </footer>
      </div>
    </div>
  );
}

export function RollbackSheet({
  files,
  onCancel,
  onApply,
}: {
  files: ResumeFile[];
  onCancel: () => void;
  onApply: (choice: "proceed" | "skip") => void;
}) {
  const [choice, setChoice] = useState<"proceed" | "skip">("proceed");
  const deleted = files.filter((file) => isDeletedDetail(file.detail, file.deleted)).length;
  const modified = files.length - deleted;
  return (
    <div className="mask" data-screen="22" onClick={onCancel}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="warn"><AlertTriangle size={22} /> 回滚发现 {files.length} 个冲突文件</h1>
        <p className="sub">{rollbackConflictSubtitle(deleted > 0)}</p>
        <FileList files={files} />
        <label className={choice === "proceed" ? "choice on" : "choice"}>
          <input type="radio" name="rollback" checked={choice === "proceed"} onChange={() => setChoice("proceed")} />
          <span>{rollbackProceedLabel(deleted > 0)}<small>{rollbackProceedNote(modified, deleted)}</small></span>
        </label>
        <label className={choice === "skip" ? "choice on" : "choice"}>
          <input type="radio" name="rollback" checked={choice === "skip"} onChange={() => setChoice("skip")} />
          <span>跳过冲突的文件<small>{rollbackSkipNote(files.length, deleted)}</small></span>
        </label>
        <footer>
          <button type="button" className="ghost" onClick={onCancel}>取消</button>
          <button type="button" className="primary" onClick={() => onApply(choice)}>继续</button>
        </footer>
      </div>
    </div>
  );
}

export function PendingSheet({
  onCancel,
  onApply,
}: {
  onCancel: () => void;
  onApply: (choice: "skip" | "overwrite") => void;
}) {
  const [choice, setChoice] = useState<"skip" | "overwrite">("skip");
  const files = PENDING_FILES;
  return (
    <div className="mask" data-screen="25" onClick={onCancel}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="warn"><AlertTriangle size={22} /> {pendingChangedTitle(files.length)}</h1>
        <p className="sub">这些文件在升级停下之后被修改，继续前先确认怎么处理</p>
        <FileList files={files} />
        <label className={choice === "skip" ? "choice on" : "choice"}>
          <input type="radio" name="pending" checked={choice === "skip"} onChange={() => setChoice("skip")} />
          <span>{SKIP_PENDING}<small>{skipPendingNote(files.length)}</small></span>
        </label>
        <label className={choice === "overwrite" ? "choice on" : "choice"}>
          <input type="radio" name="pending" checked={choice === "overwrite"} onChange={() => setChoice("overwrite")} />
          <span>{OVERWRITE_PENDING}<small>{OVERWRITE_PENDING_NOTE}</small></span>
        </label>
        <p className="foot">{CANCEL_FOOTER}</p>
        <footer>
          <button type="button" className="ghost" onClick={onCancel}>取消</button>
          <button type="button" className="primary" onClick={() => onApply(choice)}>继续</button>
        </footer>
      </div>
    </div>
  );
}

export function RollbackResumeSheet({ onLater, onContinue }: { onLater: () => void; onContinue: () => void }) {
  return (
    <div className="mask" data-screen="24">
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h1 className="warn"><Clock size={22} /> 有一次回滚没有完成</h1>
        <p className="sub">{ROLLBACK_PROGRESS.when} · {ROLLBACK_PROGRESS.where}</p>
        <div className="infocard">
          <p>已还原 {ROLLBACK_PROGRESS.done} / {ROLLBACK_PROGRESS.total} 个文件</p>
          <p className="sidetext">{ROLLBACK_PROGRESS.side}</p>
        </div>
        <p className="explain">{ROLLBACK_RESUME_BODY}</p>
        <footer>
          <button type="button" className="ghost" onClick={onLater}>稍后处理</button>
          <button type="button" className="primary" onClick={onContinue}>继续回滚</button>
        </footer>
      </div>
    </div>
  );
}
