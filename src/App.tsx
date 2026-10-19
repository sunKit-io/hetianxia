import { useEffect, useMemo, useState } from "react";
import { Server, ShieldAlert, KeyRound, AlertTriangle } from "lucide-react";
import { appBakDir, commitRemoteOverwrite, journalRuns, remoteWritePrompt, rollbackPreview, rollbackRun, resumeRun, startupFailure, startupOffers, type JournalRun } from "./backend";
import { externalChoice, keepSource, pendingChoice, rollbackChoice, type ResumeChoice, type RollbackChoice } from "./choices";
import { DiffEditor } from "./editor";
import { afterResume, engineCode, formatEngineError, formatOffer, formatReport, type ConflictFile, type RecoveryOffer } from "./engine";
import { formatFileCounts, formatRunTime, historyContinue, historyRowButtons, historyStatusLabel, REMOTE_ROLLBACK_TITLE, type HistoryContinue } from "./historyRows";
import { ALPINE, UNKNOWN, UBUNTU, canContinue, connectionSubtitle, systemTypeLabel, warningBanner, type RemoteOs } from "./os";
import { Text } from "./plain";
import { VERIFY_FAILED_BUTTON, VERIFY_FAILED_TITLE, settleRemoteWrite, type PendingRemoteWrite } from "./remoteWrite";
import {
  DETAIL_DELETED,
  DETAIL_MODIFIED,
  OVERWRITE_PENDING,
  OVERWRITE_PENDING_NOTE,
  RESUME_AGAIN,
  RESUME_NOTE,
  SKIP_PENDING,
  STOP_BODY,
  STOP_FOOTER,
  pendingChangedTitle,
  resumeConflictScreen,
  resumeConflictTitle,
  rollbackProceedLabel,
  rollbackProceedNote,
  rollbackSkipNote,
  type ResumeFile,
} from "./resumeCopy";

type Page = "edit" | "ssh" | "preview" | "history";
type Sheet = null | "rollback" | "log" | "failure" | "startup" | "external" | "pending" | "rollbackResume";

type Host = {
  id: string;
  name: string;
  user: string;
  host: string;
  port: number;
  os: RemoteOs;
  proxy?: boolean;
  dot: "ok" | "bad" | "off";
  key: string;
  fingerprint: string;
  seen: string;
};

const HOSTS: Host[] = [
  {
    id: "build-01",
    name: "build-01",
    user: "deploy",
    host: "10.0.8.15",
    port: 22,
    os: ALPINE,
    dot: "ok",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:q7Xk2mR9vT0pL+3nYd8wHcF1sZ4aE6uJbN5oK1M2gQw",
    seen: "ED25519 · 9月02日首次确认并记住",
  },
  {
    id: "prod-gw-01",
    name: "prod-gw-01",
    user: "ops",
    host: "172.16.2.40",
    port: 22,
    os: UBUNTU,
    dot: "ok",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:aa",
    seen: "ED25519 · 8月01日首次确认并记住",
  },
  {
    id: "edge-01",
    name: "edge-01",
    user: "deploy",
    host: "10.0.8.15",
    port: 22,
    os: UNKNOWN,
    dot: "ok",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:q7Xk2mR9vT0pL+3nYd8wHcF1sZ4aE6uJbN5oK1M2gQw",
    seen: "ED25519 · 9月02日首次确认并记住",
  },
  {
    id: "intranet-jump",
    name: "intranet-jump",
    user: "ops",
    host: "10.1.0.4",
    port: 22,
    os: UBUNTU,
    proxy: true,
    dot: "off",
    key: "",
    fingerprint: "",
    seen: "",
  },
];

const EXTERNAL_FILES: ResumeFile[] = [
  { path: "src/routes.js", detail: DETAIL_MODIFIED },
  { path: "public/old-banner.png", detail: DETAIL_DELETED },
];

const PENDING_FILES = ["src/legacy/cache.js", "config/local.env"];

export function App() {
  const [page, setPage] = useState<Page>("edit");
  const [hostId, setHostId] = useState("build-01");
  const [allowReadonly, setAllowReadonly] = useState(false);
  const [sheet, setSheet] = useState<Sheet>(null);
  const [remoteTx, setRemoteTx] = useState<PendingRemoteWrite | null>(null);
  const [verifyBody, setVerifyBody] = useState<string | null>(null);
  const [bakDir, setBakDir] = useState<string | null>(null);
  const [cleared, setCleared] = useState<Record<string, boolean>>({});
  const [flowId, setFlowId] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [offer, setOffer] = useState<RecoveryOffer | null>(null);
  const [bootError, setBootError] = useState<string | null>(null);
  const [externalFiles, setExternalFiles] = useState<ConflictFile[]>(EXTERNAL_FILES);
  const [externalTitle, setExternalTitle] = useState(resumeConflictTitle(EXTERNAL_FILES));
  const [pendingFiles, setPendingFiles] = useState<ConflictFile[]>(PENDING_FILES.map((path) => ({ path, detail: DETAIL_MODIFIED })));
  const [rollbackFiles, setRollbackFiles] = useState<ConflictFile[]>([]);
  const [rollbackAt, setRollbackAt] = useState(0);
  const host = HOSTS.find((h) => h.id === hostId) ?? HOSTS[0];

  useEffect(() => {
    void appBakDir().then(setBakDir).catch((err) => {
      const message = formatEngineError(err);
      if (message.startsWith("E_")) setStatus(message);
    });
    void startupFailure().then((message) => {
      if (message) setBootError(message);
    }).catch((err) => {
      const message = formatEngineError(err);
      if (message.startsWith("E_")) setBootError(message);
    });
    void startupOffers().then((offers) => {
      const first = offers[0];
      if (!first) return;
      setOffer(first);
      setFlowId(first.execId);
      setStatus(formatOffer(first));
      if (first.state === "ROLLING_BACK" || first.state === "ROLLBACK_PARTIAL") setSheet("rollbackResume");
      else setSheet("startup");
    }).catch((err) => {
      const message = formatEngineError(err);
      if (message.startsWith("E_")) setStatus(message);
    });
  }, []);

  async function applyResume(id: string, choice: ResumeChoice) {
    setFlowId(id);
    try {
      const next = afterResume(choice, await resumeRun(id, choice));
      if (next.sheet === "external") {
        setExternalTitle(next.title);
        setExternalFiles(next.files);
      } else if (next.sheet === "pending") {
        setPendingFiles(next.files);
      } else if (next.sheet === "rollback") {
        setRollbackFiles(next.files);
      }
      setStatus(next.status);
      setSheet(next.sheet);
    } catch (err) {
      const message = formatEngineError(err);
      setStatus(message);
      if (choice === "rollback" && message.startsWith("E_INVALID_STATE")) setSheet("rollback");
    }
  }

  async function openRollback(execId: string) {
    setFlowId(execId);
    try {
      const preview = await rollbackPreview(execId);
      setRollbackFiles(preview.files);
      setRollbackAt(preview.startedAt);
      setSheet("rollback");
    } catch (err) {
      setStatus(formatEngineError(err));
    }
  }

  async function applyRollback(id: string, choice: RollbackChoice) {
    setFlowId(id);
    try {
      const report = await rollbackRun(id, choice);
      setStatus(formatReport(report));
      setSheet(null);
    } catch (err) {
      const message = formatEngineError(err);
      setStatus(message);
      if (choice === "cancel") setSheet(null);
    }
  }

  return (
    <div className="desktop">
      <div className="window">
        <header className="titlebar">
          <span className="lights" aria-hidden>
            <i /><i /><i />
          </span>
          <div className="titleblock">
            <strong>{page === "ssh" ? "SSH 连接" : page === "history" ? "历史记录" : ""}</strong>
            <span>{page === "ssh" ? `${HOSTS.length} 个连接` : page === "history" ? "保留最近 10 次 · 超过 30 天自动清理" : ""}</span>
          </div>
          <nav className="pages">
            <button className={page === "edit" ? "on" : ""} onClick={() => setPage("edit")}>编辑</button>
            <button className={page === "ssh" ? "on" : ""} onClick={() => setPage("ssh")}>连接</button>
            <button className={page === "preview" ? "on" : ""} onClick={() => setPage("preview")}>预演</button>
            <button className={page === "history" ? "on" : ""} onClick={() => setPage("history")}>历史</button>
          </nav>
          {page === "ssh" && <button className="ghost import">从 ~/.ssh/config 导入</button>}
        </header>
        {status && !sheet && <p className={status.includes("E_") ? "status err" : "status"} role="status">{status}</p>}
        {page === "edit" && <DiffEditor onWriteFailed={() => setSheet("failure")} />}
        {page === "ssh" && (
          <SshPage
            host={host}
            cleared={!!cleared[host.id]}
            onPick={setHostId}
            onClear={() => setCleared((s) => ({ ...s, [host.id]: true }))}
          />
        )}
        {page === "preview" && (
          <PreviewPage
            allowReadonly={allowReadonly}
            setAllowReadonly={setAllowReadonly}
            bakDir={bakDir}
            onRemoteWrite={() => {
              const remotePath = "/opt/gateway/app.conf";
              void remoteWritePrompt(host.host, host.port, remotePath).then((prompt) => {
                setRemoteTx({
                  execId: prompt.execId,
                  backupDir: prompt.backupDir,
                  title: prompt.title,
                  body: prompt.body,
                  cancelLabel: prompt.cancelLabel,
                  proceedLabel: prompt.proceedLabel,
                  defaultButton: prompt.defaultButton,
                  host: host.host,
                  port: host.port,
                  user: host.user,
                  account: host.id,
                  keyPath: host.key,
                  remotePath,
                });
              }).catch((err) => setStatus(formatEngineError(err)));
            }}
          />
        )}
        {page === "history" && (
          <HistoryPage
            onRollback={(execId) => void openRollback(execId)}
            onContinue={(action) => void applyResume(action.execId, action.choice)}
          />
        )}
        {sheet === "rollback" && (
          <RollbackSheet
            files={rollbackFiles}
            startedAt={rollbackAt}
            alert={status}
            onCancel={() => void applyRollback(flowId, "cancel")}
            onApply={(choice) => void applyRollback(flowId, rollbackChoice(choice))}
          />
        )}
        {sheet === "log" && <LogSheet alert={status} onClose={() => void applyResume(flowId, "cancel")} onRollback={() => void applyResume(flowId, "rollback")} />}
        {sheet === "failure" && <WriteFailureSheet alert={status} offer={offer} onLater={() => void applyResume(flowId, "cancel")} onRollback={() => void applyResume(flowId, "rollback")} />}
        {sheet === "startup" && (
          <StartupSheet
            alert={status}
            offer={offer}
            onLater={() => offer && void applyResume(offer.execId, "cancel")}
            onRollback={() => offer && void openRollback(offer.execId)}
            onContinue={() => offer && void applyResume(offer.execId, "continue")}
          />
        )}
        {sheet === "external" && (
          <ExternalSheet
            title={externalTitle}
            files={externalFiles}
            alert={status}
            onClose={() => void applyResume(flowId, "cancel")}
            onKeep={() => void applyResume(flowId, externalChoice("keep"))}
            onRollback={() => void applyResume(flowId, externalChoice("rollback"))}
          />
        )}
        {sheet === "pending" && (
          <PendingSheet files={pendingFiles} alert={status} onCancel={() => void applyResume(flowId, "cancel")} onApply={(choice) => void applyResume(flowId, pendingChoice(choice))} />
        )}
        {sheet === "rollbackResume" && (
          <RollbackResumeSheet alert={status} offer={offer} onLater={() => offer && void applyResume(offer.execId, "cancel")} onContinue={() => offer && void applyRollback(offer.execId, "proceedBackupCurrent")} />
        )}
        {bootError && (
          <div className="mask" role="alertdialog" aria-modal="true" aria-labelledby="boot-error-title">
            <div className="sheet small">
              <h1 id="boot-error-title"><AlertTriangle size={18} /> 启动失败</h1>
              <p>运行锁或事务日志没有准备好，升级和回滚都不会开始。</p>
              <p className="err" role="alert"><Text value={bootError} /></p>
              <footer>
                <button className="primary" onClick={() => setBootError(null)}>知道了</button>
              </footer>
            </div>
          </div>
        )}
        {remoteTx && (
          <RemoteWriteSheet
            pending={remoteTx}
            onCancel={() => {
              settleRemoteWrite("取消", () => {
                throw new Error("取消不能写入");
              });
              setRemoteTx(null);
            }}
            onProceed={() => {
              const pending = remoteTx;
              settleRemoteWrite("仍然写入", () => {
                setRemoteTx(null);
                const bytes = Array.from(new TextEncoder().encode("gateway-1.8"));
                void commitRemoteOverwrite({
                  execId: pending.execId,
                  host: pending.host,
                  port: pending.port,
                  user: pending.user,
                  remotePath: pending.remotePath,
                  bytes,
                  auth: "key",
                  account: pending.account,
                  keyPath: pending.keyPath,
                }).then((outcome) => {
                  setStatus(`已写入 ${outcome.remotePath}`);
                }).catch((err) => {
                  const rec = err && typeof err === "object" ? err as Record<string, unknown> : {};
                  const code = rec.code != null ? engineCode(String(rec.code)) : "";
                  if (code === "E_VERIFY_FAILED" && rec.message != null) {
                    setVerifyBody(String(rec.message));
                    return;
                  }
                  setStatus(formatEngineError(err));
                });
              });
            }}
          />
        )}
        {verifyBody && <VerifyFailedSheet body={verifyBody} onClose={() => setVerifyBody(null)} />}
      </div>
    </div>
  );
}

function SshPage({ host, cleared, onPick, onClear }: { host: Host; cleared: boolean; onPick: (id: string) => void; onClear: () => void }) {
  const banner = warningBanner(host.os);
  const usable = canContinue(host.os) && !host.proxy;
  const subtitle = connectionSubtitle(host.os);
  const systemRow = systemTypeLabel(host.os);
  return (
    <div className="ssh">
      <aside>
        <input className="search" placeholder="搜索连接" />
        {HOSTS.map((h) => (
          <button key={h.id} className={`host ${h.id === host.id ? "sel" : ""} ${h.proxy ? "disabled" : ""}`} onClick={() => onPick(h.id)} disabled={false}>
            <Server size={16} />
            <span>
              <b>{h.name}</b>
              <em>{h.proxy ? "需要跳板机，第二版支持" : `${h.user}@${h.host}:${h.port}`}</em>
            </span>
            <i className={`dot ${h.dot}`} />
          </button>
        ))}
      </aside>
      <section className={host.proxy ? "dim" : ""}>
        <div className="hosthead">
          <Server size={18} />
          <div>
            <h1>{host.name}</h1>
            <p>
              上次连接今天 11:42 · <span className={host.os.kind === "unidentified" || (host.os.kind === "linux" && !host.os.verified) ? "warn" : ""}>{subtitle}</span>
            </p>
          </div>
          <button className="ghost" disabled={!usable}>测试连接</button>
        </div>
        <label>名称<input defaultValue={host.name} disabled={host.proxy} /></label>
        <label>主机<input defaultValue={host.host} disabled={host.proxy} /></label>
        <div className="row2">
          <label>端口<input defaultValue={host.port} disabled={host.proxy} /></label>
          <label>用户名<input defaultValue={host.user} disabled={host.proxy} /></label>
        </div>
        <div className="auth">认证方式 <span>密码</span><span className="on">私钥文件</span><span>ssh-agent</span></div>
        <label>私钥文件<input defaultValue={host.key} disabled={host.proxy} /></label>
        <label>私钥口令<input type="password" placeholder="已存入钥匙串" disabled={host.proxy} /></label>
        <div className="savekey"><label><input type="checkbox" defaultChecked disabled={host.proxy} /> 保存到钥匙串</label><span className="ok">已存入钥匙串</span></div>
        <p className="keyhint">钥匙串服务名 com.hetianxia.app.ssh，账号是连接 id「{host.id}」。</p>
        {banner && usable && (
          <div className="banner"><AlertTriangle size={16} /> {banner}</div>
        )}
        {host.proxy && <div className="banner">需要跳板机，第二版支持。这一版不能用这个连接。</div>}
        <h2>连接测试</h2>
        <ul className="checks">
          <li><span>TCP 连接</span><em>38 ms</em></li>
          <li><span>身份认证</span><em>私钥 ed25519</em></li>
          <li><span>SFTP 子系统</span><em>可用</em></li>
          <li><span>服务器哈希加速</span><em>sha256sum 可用</em></li>
          <li><span>系统类型</span><em>{systemRow}</em></li>
        </ul>
        <h2>主机指纹</h2>
        <div className="fp">
          <div>
            <b>{cleared ? "尚未记住" : host.fingerprint}</b>
            <em>{cleared ? "下次连接按首次连接，需要重新确认指纹" : host.seen}</em>
          </div>
          <button className="link" onClick={onClear} disabled={host.proxy || cleared}>清除指纹</button>
        </div>
        <footer>
          <button className="danger" disabled={host.proxy}>删除连接</button>
          <span />
          <button className="ghost">取消</button>
          <button className="primary" disabled={!usable}>保存</button>
        </footer>
      </section>
    </div>
  );
}

function PreviewPage({ allowReadonly, setAllowReadonly, bakDir, onRemoteWrite }: { allowReadonly: boolean; setAllowReadonly: (v: boolean) => void; bakDir: string | null; onRemoteWrite: () => void }) {
  const [target, setTarget] = useState<"local" | "remote">("local");
  const counts = useMemo(() => {
    const overwrite = allowReadonly ? 13 : 11;
    const skip = allowReadonly ? 0 : 2;
    return { overwrite, skip };
  }, [allowReadonly]);
  return (
    <div className="sheetwrap">
      <div className="sheet">
        <h1><ShieldAlert size={18} /> 确认执行升级</h1>
        <p className="sub">{target === "remote" ? "OrderCenter · build-01:/opt/gateway ← gateway-1.8" : "OrderCenter · 本机 ~/work/order-center ← release-3.0.0.zip"}</p>
        <div className="auth">写入到 <span className={target === "local" ? "on" : ""} onClick={() => setTarget("local")}>本机</span><span className={target === "remote" ? "on" : ""} onClick={() => setTarget("remote")}>远程</span></div>
        <div className="stats">
          <b>6<small>新增</small></b>
          <b>{counts.overwrite}<small>覆盖{counts.skip ? "" : " · 其中合并 4"}</small></b>
          <b>1<small>移动</small></b>
          <b>0<small>删除</small></b>
          {counts.skip > 0 && <b className="skip">{counts.skip}<small>跳过（只读）</small></b>}
        </div>
        <ul className="changes">
          <li><i className="add" /> <Text value="plugins/tracing/index.ts" /> <em>升级包</em></li>
          <li><i className="mod" /> <Text value="src/config/app.config.js" /> <em>合并结果</em></li>
          <li data-keep-source={keepSource("移动（保留旧文件）") ? "1" : "0"}><i className="move" /> <Text value="lib/format.ts → utils/format.ts" /> <em>移动（保留旧文件）</em></li>
        </ul>
        <div className="ro">
          <b>只读文件（2）</b>
          <div>bin/start.sh <em>E_PERMISSION · {allowReadonly ? "将临时去掉只读" : "不勾选将跳过"}</em></div>
          <div>conf/license.key <em>E_PERMISSION · {allowReadonly ? "将临时去掉只读" : "不勾选将跳过"}</em></div>
          <label><input type="checkbox" checked={allowReadonly} onChange={(e) => setAllowReadonly(e.target.checked)} /> 覆盖只读文件</label>
          <p>临时去掉只读属性，写入后恢复。</p>
        </div>
        <label className="bak"><input type="checkbox" defaultChecked /> 执行前备份到 {bakDir ? <Text value={bakDir} /> : "…"}</label>
        <footer>
          <button className="primary" onClick={() => { if (target === "remote") onRemoteWrite(); }}>{target === "remote" ? "写入远程" : "备份并执行"}</button>
        </footer>
      </div>
    </div>
  );
}

function HistoryPage({ onRollback, onContinue }: { onRollback: (execId: string) => void; onContinue: (action: HistoryContinue) => void }) {
  const [runs, setRuns] = useState<JournalRun[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  useEffect(() => {
    void journalRuns()
      .then((rows) => {
        setRuns(rows);
        setLoadError(null);
      })
      .catch((err) => setLoadError(formatEngineError(err)));
  }, []);
  return (
    <div className="history" data-screen="26">
      {loadError ? <p className="err" role="alert"><Text value={loadError} /></p> : runs.length === 0 ? <p>还没有升级记录</p> : null}
      <table>
        <thead><tr><th>时间</th><th>位置</th><th>变更</th><th>结果</th><th /></tr></thead>
        <tbody>
          {runs.map((run) => {
            const label = historyStatusLabel(run.status);
            const buttons = historyRowButtons(label);
            return (
            <tr key={run.execId}>
              <td><Text value={formatRunTime(run.startedAt)} /></td>
              <td><Text value={run.projectRoot ?? ""} /></td>
              <td><Text value={formatFileCounts(run)} /></td>
              <td><Text value={label} /></td>
              <td>
                {run.remote ? (
                  <button type="button" disabled title={REMOTE_ROLLBACK_TITLE}>回滚</button>
                ) : (
                  <>
                    {buttons.rollback && <button type="button" onClick={() => onRollback(run.execId)}>回滚…</button>}
                    {buttons.resume && <button type="button" onClick={() => onContinue(historyContinue(run.execId))}>继续</button>}
                  </>
                )}
              </td>
            </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function VerifyFailedSheet({ body, onClose }: { body: string; onClose: () => void }) {
  return (
    <div className="mask" onClick={onClose} onKeyDown={(event) => { if (event.key === "Escape") onClose(); }}>
      <div className="sheet small" role="dialog" aria-modal="true" aria-labelledby="verify-failed-title" onClick={(event) => event.stopPropagation()}>
        <h1 id="verify-failed-title"><AlertTriangle size={18} /> <Text value={VERIFY_FAILED_TITLE} /></h1>
        <p><Text value={body} /></p>
        <footer>
          <span />
          <button type="button" className="primary" autoFocus onClick={onClose}>{VERIFY_FAILED_BUTTON}</button>
        </footer>
      </div>
    </div>
  );
}

function RemoteWriteSheet({ pending, onCancel, onProceed }: { pending: PendingRemoteWrite; onCancel: () => void; onProceed: () => void }) {
  return (
    <div className="mask" onClick={onCancel} onKeyDown={(event) => { if (event.key === "Escape") onCancel(); }}>
      <div className="sheet small" role="dialog" aria-modal="true" aria-labelledby="remote-write-title" onClick={(event) => event.stopPropagation()}>
        <h1 id="remote-write-title"><AlertTriangle size={18} /> <Text value={pending.title} /></h1>
        <p><Text value={pending.body} /></p>
        <footer>
          <span />
          <button type="button" className="ghost" onClick={onProceed}>{pending.proceedLabel}</button>
          <button type="button" className="primary" autoFocus data-default="true" onClick={onCancel}>{pending.cancelLabel}</button>
        </footer>
      </div>
    </div>
  );
}

function RollbackSheet({ files, startedAt, alert, onCancel, onApply }: { files: ConflictFile[]; startedAt: number; alert: string | null; onCancel: () => void; onApply: (choice: "proceed" | "skip") => void }) {
  const [choice, setChoice] = useState("proceed");
  const hasDeletion = files.some((file) => file.detail === DETAIL_DELETED);
  const when = formatRunTime(startedAt);
  return (
    <div className="mask" data-screen={hasDeletion ? "22" : "11"}>
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> 回滚发现 {files.length} 个冲突文件</h1>
        <p>{when ? `这些文件在 ${when} 升级后又被改动过。` : "这些文件在升级后又被改动过。"}</p>
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        <ul className="changes">
          {files.map((file) => (
            <li key={file.path} className={file.detail === DETAIL_DELETED ? "deleted" : ""}>
              <span><Text value={file.path} /></span>
              <em><Text value={file.detail} /></em>
            </li>
          ))}
        </ul>
        <label className={choice === "proceed" ? "choice on" : "choice"}><input type="radio" name="rollback" checked={choice === "proceed"} onChange={() => setChoice("proceed")} /> {rollbackProceedLabel(hasDeletion)}<small>{rollbackProceedNote(hasDeletion)}</small></label>
        <label className={choice === "skip" ? "choice on" : "choice"}><input type="radio" name="rollback" checked={choice === "skip"} onChange={() => setChoice("skip")} /> 跳过冲突的文件<small>{rollbackSkipNote(hasDeletion)}</small></label>
        <footer>
          <button className="ghost" onClick={onCancel}>取消</button>
          <button className="primary" onClick={() => onApply(choice === "skip" ? "skip" : "proceed")}>继续</button>
        </footer>
      </div>
    </div>
  );
}

function WriteFailureSheet({ alert, offer, onLater, onRollback }: { alert: string | null; offer: RecoveryOffer | null; onLater: () => void; onRollback: () => void }) {
  const file = offer?.conflicts[0]?.path ?? "";
  const code = offer?.code ? engineCode(offer.code) : "";
  const message = offer?.error ?? "";
  const detail = [code, message].filter(Boolean).join(" · ");
  return (
    <div className="mask" data-screen="18">
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> 升级已停止：有文件写入失败</h1>
        <p>{STOP_BODY}</p>
        {offer?.state ? <p><Text value={offer.state} /></p> : null}
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        {(file || detail) && (
          <ul className="changes">
            <li><span><Text value={file} /></span>{detail ? <em className="err"><Text value={detail} /></em> : null}</li>
          </ul>
        )}
        <p className="keyhint">{STOP_FOOTER}</p>
        <footer>
          <button className="ghost" onClick={onLater}>稍后处理</button>
          <button className="primary" onClick={onRollback}>回滚</button>
        </footer>
      </div>
    </div>
  );
}

function StartupSheet({ alert, offer, onLater, onRollback, onContinue }: { alert: string | null; offer: RecoveryOffer | null; onLater: () => void; onRollback: () => void; onContinue: () => void }) {
  const file = offer?.conflicts[0]?.path ?? "";
  const code = offer?.code ? engineCode(offer.code) : "";
  const message = offer?.error ?? "";
  const detail = [code, message].filter(Boolean).join(" · ");
  return (
    <div className="mask" data-screen="20">
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> 有一次升级没有完成</h1>
        <p>上次升级停在写入中途。可以继续、回滚，或先放着。</p>
        {offer?.state ? <p><Text value={offer.state} /></p> : null}
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        {(file || detail) && (
          <ul className="changes">
            <li><span><Text value={file} /></span>{detail ? <em className="err"><Text value={detail} /></em> : null}</li>
          </ul>
        )}
        <p className="keyhint">{RESUME_NOTE}</p>
        <footer>
          <button className="ghost" onClick={onLater}>稍后处理</button>
          <button className="ghost" onClick={onRollback}>回滚</button>
          <button className="primary" onClick={onContinue}>继续</button>
        </footer>
      </div>
    </div>
  );
}

function ExternalSheet({ title, files, alert, onClose, onKeep, onRollback }: { title: string; files: ConflictFile[]; alert: string | null; onClose: () => void; onKeep: () => void; onRollback: () => void }) {
  const [choice, setChoice] = useState("keep");
  return (
    <div className="mask" data-screen={resumeConflictScreen(files)}>
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> <Text value={title} /></h1>
        <p className="sub">继续之前先确认怎么处理这些文件</p>
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        <ul className="changes">
          {files.map((file) => (
            <li key={file.path} className={file.detail === DETAIL_DELETED ? "deleted" : ""}>
              <span><Text value={file.path} /></span>
              <em><Text value={file.detail} /></em>
            </li>
          ))}
        </ul>
        <label className={choice === "keep" ? "choice on" : "choice"}>
          <input type="radio" name="external" checked={choice === "keep"} onChange={() => setChoice("keep")} /> 保留外部修改并继续
          {files.some((file) => file.detail === DETAIL_DELETED) && <small>已经删除的文件保持删除</small>}
        </label>
        <label className={choice === "rollback" ? "choice on" : "choice"}>
          <input type="radio" name="external" checked={choice === "rollback"} onChange={() => setChoice("rollback")} /> 回滚
        </label>
        <p className="keyhint">{RESUME_AGAIN}</p>
        <footer>
          <button className="ghost" onClick={onClose}>取消</button>
          <button className="primary" onClick={() => (choice === "keep" ? onKeep() : onRollback())}>继续</button>
        </footer>
      </div>
    </div>
  );
}

function PendingSheet({ files, alert, onCancel, onApply }: { files: ConflictFile[]; alert: string | null; onCancel: () => void; onApply: (choice: "skip" | "overwrite") => void }) {
  const [choice, setChoice] = useState("skip");
  return (
    <div className="mask" data-screen="25">
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> {pendingChangedTitle(files.length)}</h1>
        <p className="sub">这些文件这次还没写入</p>
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        <ul className="changes">
          {files.map((file) => (
            <li key={file.path}><span><Text value={file.path} /></span><em><Text value={file.detail} /></em></li>
          ))}
        </ul>
        <label className={choice === "skip" ? "choice on" : "choice"}>
          <input type="radio" name="pending" checked={choice === "skip"} onChange={() => setChoice("skip")} /> {SKIP_PENDING}
        </label>
        <label className={choice === "overwrite" ? "choice on" : "choice"}>
          <input type="radio" name="pending" checked={choice === "overwrite"} onChange={() => setChoice("overwrite")} /> {OVERWRITE_PENDING}
          <small>{OVERWRITE_PENDING_NOTE}</small>
        </label>
        <footer>
          <button className="ghost" onClick={onCancel}>取消</button>
          <button className="primary" onClick={() => onApply(choice === "overwrite" ? "overwrite" : "skip")}>继续</button>
        </footer>
      </div>
    </div>
  );
}

function RollbackResumeSheet({ alert, offer, onLater, onContinue }: { alert: string | null; offer: RecoveryOffer | null; onLater: () => void; onContinue: () => void }) {
  return (
    <div className="mask" data-screen="24">
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> 有一次回滚没有完成</h1>
        <p>上次回滚停在中途。可以接着回滚，或先放着。</p>
        {offer?.state ? <p><Text value={offer.state} /></p> : null}
        {alert && <p className="err" role="alert"><Text value={alert} /></p>}
        <footer>
          <button className="ghost" onClick={onLater}>稍后处理</button>
          <button className="primary" onClick={onContinue}>继续回滚</button>
        </footer>
      </div>
    </div>
  );
}

function LogSheet({ alert, onClose, onRollback }: { alert: string | null; onClose: () => void; onRollback: () => void }) {
  return (
    <div className="mask">
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><KeyRound size={18} /> 升级已停止：事务日志写入失败</h1>
        <p>可能是磁盘空间不足或没有写权限。已停止写入，不会自动回滚。</p>
        {alert ? <p className="err"><Text value={alert} /></p> : null}
        <p>备份清单 manifest.json 仍保留已备份的文件。选「稍后处理」后，下次启动会继续提示这次未完成的升级。</p>
        <footer>
          <button className="ghost" onClick={onClose}>稍后处理</button>
          <button className="primary" onClick={onRollback}>回滚</button>
        </footer>
      </div>
    </div>
  );
}
