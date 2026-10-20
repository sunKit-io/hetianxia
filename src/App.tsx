import { useEffect, useMemo, useRef, useState } from "react";
import { Server, ShieldAlert, KeyRound, AlertTriangle } from "lucide-react";
import { appBakDir, clearConnectionFingerprint, commitRemoteOverwrite, confirmExecute, deleteConnection, deleteSshSecret, executeLocal, journalRuns, listConnections, listRemoteDirs, loadFilePicks, mergeTexts, pickProjectDirectory, pickUpgradeDirectory, pickUpgradeZip, readSide, remoteWritePrompt, rollbackPreview, rollbackRun, resumeRun, saveConnection, saveFilePicks, saveSshSecret, scanPair, sshSecretStatus, startupFailure, startupOffers, testConnection, type JournalRun, type ProbeReport, type SavedConnection } from "./backend";
import { browseCrumbs, joinChild } from "./remoteBrowse";
import { invertPicked, pickedFromSkipped, skippedOf } from "./filePicks";
import { confirmExecuteBody, settleExecuteConfirm, type ExecuteConfirm } from "./confirmExecute";
import { externalChoice, keepSource, pendingChoice, rollbackChoice, type ResumeChoice, type RollbackChoice } from "./choices";
import { DiffEditor } from "./editor";
import { afterResume, engineCode, formatEngineError, formatFileResult, formatOffer, formatReport, type ConflictFile, type ExecReport, type RecoveryOffer } from "./engine";
import {
  attachFlowStatus,
  baseName,
  buildLocalPlan,
  clearFileNotes,
  decisionMap,
  editedContents,
  execPlanFor,
  firstTextFile,
  newExecId,
  noteShownFile,
  applyCheck,
  checkState,
  filePathsUnder,
  planListRows,
  previewCounts,
  selectedPlan,
  selectionSummary,
  type CheckState,
  type ExecPlan,
  type LocalPlan,
} from "./localPlan";
import { formatFileCounts, formatRunTime, historyContinue, historyRowButtons, historyStatusLabel, REMOTE_ROLLBACK_TITLE, type HistoryContinue } from "./historyRows";
import { Text } from "./plain";
import { remoteRootFieldError } from "./remoteRoot";
import { VERIFY_FAILED_BUTTON, VERIFY_FAILED_TITLE, connectionRemoteRoot, currentRemotePlan, isAbsoluteRemoteRoot, isFilesystemRemoteRoot, noteRemoteRoot, remoteBatchFailure, remoteFileMark, remotePlanFiles, settleRemoteWrite, type PendingRemoteWrite, type RemoteBatchReport } from "./remoteWrite";
import {
  DETAIL_DELETED,
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

function blankConnection(): SavedConnection {
  return { id: "", name: "", host: "", port: 22, user: "", auth: "password", keyPath: "", fingerprint: "", remoteRoot: null };
}

const EXTERNAL_FILES: ResumeFile[] = [];

const PENDING_FILES: ConflictFile[] = [];

export function App() {
  const [page, setPage] = useState<Page>("edit");
  const [connections, setConnections] = useState<SavedConnection[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [allowReadonly, setAllowReadonly] = useState(false);
  const [sheet, setSheet] = useState<Sheet>(null);
  const [historyToken, setHistoryToken] = useState(0);
  const [remoteTx, setRemoteTx] = useState<PendingRemoteWrite | null>(null);
  const [remoteBatch, setRemoteBatch] = useState<RemoteBatchReport | null>(null);
  const [remoteProject, setRemoteProject] = useState<string | null>(null);
  const [remoteBrowser, setRemoteBrowser] = useState<{ path: string; dirs: string[]; reading: boolean; error: string | null } | null>(null);
  const [verifyBody, setVerifyBody] = useState<string | null>(null);
  const [bakDir, setBakDir] = useState<string | null>(null);
  const [flowId, setFlowId] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [offer, setOffer] = useState<RecoveryOffer | null>(null);
  const [bootError, setBootError] = useState<string | null>(null);
  const [externalFiles, setExternalFiles] = useState<ConflictFile[]>(EXTERNAL_FILES);
  const [externalTitle, setExternalTitle] = useState(resumeConflictTitle(EXTERNAL_FILES));
  const [pendingFiles, setPendingFiles] = useState<ConflictFile[]>(PENDING_FILES);
  const [rollbackFiles, setRollbackFiles] = useState<ConflictFile[]>([]);
  const [rollbackAt, setRollbackAt] = useState(0);
  const [projectRoot, setProjectRoot] = useState<string | null>(null);
  const [packagePath, setPackagePath] = useState<string | null>(null);
  const [packageSource, setPackageSource] = useState<string | null>(null);
  const [plan, setPlan] = useState<LocalPlan | null>(null);
  const [pickedPaths, setPickedPaths] = useState<ReadonlySet<string>>(new Set());
  const [currentFile, setCurrentFile] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<ExecReport | null>(null);
  const [pendingExec, setPendingExec] = useState<{ root: string; plan: ExecPlan; allowReadonly: boolean; preview: ExecuteConfirm } | null>(null);
  const selected = connections.find((item) => item.id === selectedId) ?? null;

  useEffect(() => {
    void listConnections().then(setConnections).catch((err) => setStatus(formatEngineError(err)));
  }, []);

  useEffect(() => {
    noteShownFile(plan, currentFile);
  }, [plan, currentFile]);

  useEffect(() => {
    setRemoteProject(null);
    setRemoteBrowser(null);
  }, [selectedId]);

  useEffect(() => {
    noteRemoteRoot(remoteProject || connectionRemoteRoot(selected));
  }, [selected, remoteProject]);

  useEffect(() => attachFlowStatus(setStatus), []);

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

  async function refreshPlan(project: string, prepared: string, source: string) {
    setScanning(true);
    setReport(null);
    setRemoteBatch(null);
    clearFileNotes();
    try {
      const scanned = await scanPair(project, prepared);
      const next = await buildLocalPlan({
        projectRoot: project,
        packagePath: prepared,
        files: scanned.files,
        read: readSide,
        mergeText: mergeTexts,
      });
      setPlan(next);
      const paths = next.files.map((file) => file.path.replace(/\\/g, "/"));
      let skipped: string[] = [];
      try {
        skipped = await loadFilePicks(project, source);
        setStatus(null);
      } catch (err) {
        setStatus(formatEngineError(err));
      }
      setPickedPaths(pickedFromSkipped(paths, skipped));
      setCurrentFile(firstTextFile(next));
    } catch (err) {
      setPlan(null);
      setPickedPaths(new Set());
      setCurrentFile(null);
      setStatus(formatEngineError(err));
    } finally {
      setScanning(false);
    }
  }

  function applyPicked(next: ReadonlySet<string>) {
    setPickedPaths(next);
    if (!projectRoot || !packageSource || !plan) return;
    const paths = plan.files.map((file) => file.path.replace(/\\/g, "/"));
    void saveFilePicks(projectRoot, packageSource, skippedOf(paths, next)).catch((err) => setStatus(formatEngineError(err)));
  }

  async function onPickProject() {
    try {
      const picked = await pickProjectDirectory();
      if (!picked) return;
      setProjectRoot(picked);
      if (packagePath && packageSource) await refreshPlan(picked, packagePath, packageSource);
    } catch (err) {
      setStatus(formatEngineError(err));
    }
  }

  async function onPickPackage(pick: () => Promise<{ source: string; prepared: string } | null>) {
    try {
      const picked = await pick();
      if (!picked) return;
      setPackageSource(picked.source);
      setPackagePath(picked.prepared);
      if (projectRoot) await refreshPlan(projectRoot, picked.prepared, picked.source);
    } catch (err) {
      setStatus(formatEngineError(err));
    }
  }

  async function backupAndExecute(edits: Record<string, string>) {
    if (!projectRoot || !plan || running || pendingExec) return;
    const chosen = selectedPlan(plan, pickedPaths);
    if (chosen.files.length === 0) return;
    try {
      const execPlan = await execPlanFor(chosen, edits, decisionMap(), mergeTexts, newExecId());
      const preview = await confirmExecute(projectRoot, execPlan, allowReadonly);
      setPendingExec({ root: projectRoot, plan: execPlan, allowReadonly, preview });
    } catch (err) {
      setStatus(formatEngineError(err));
    }
  }

  function dismissExecute(action: "cancel" | "escape") {
    if (action === "escape") settleExecuteConfirm("escape", () => { void runConfirmed(); });
    else settleExecuteConfirm("cancel", () => { void runConfirmed(); });
    setPendingExec(null);
  }

  function acceptExecute() {
    settleExecuteConfirm("confirm", () => { void runConfirmed(); });
  }

  async function runConfirmed() {
    const current = pendingExec;
    if (!current || running) return;
    setPendingExec(null);
    setRunning(true);
    setStatus("正在备份并执行…");
    try {
      const result = await executeLocal(current.root, current.plan, current.allowReadonly, current.preview.token);
      setReport(result);
      setStatus(formatReport(result));
    } catch (err) {
      setStatus(formatEngineError(err));
    } finally {
      setRunning(false);
    }
  }

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
      setHistoryToken((token) => token + 1);
      setStatus(formatReport(report));
      setSheet(null);
    } catch (err) {
      const message = formatEngineError(err);
      setStatus(message);
      if (choice === "cancel") setSheet(null);
    }
  }

  noteShownFile(plan, currentFile);
  noteRemoteRoot(remoteProject || connectionRemoteRoot(selected));

  async function loadRemoteDir(path: string) {
    if (!selected) return;
    setRemoteBrowser({ path, dirs: [], reading: true, error: null });
    try {
      const listed = await listRemoteDirs(selected.id, path);
      setRemoteBrowser({ path: listed.path, dirs: listed.dirs, reading: false, error: null });
    } catch (err) {
      setRemoteBrowser({ path, dirs: [], reading: false, error: formatEngineError(err) });
    }
  }

  function openRemoteBrowser() {
    if (!selected) {
      setStatus("先保存并选择一个连接");
      return;
    }
    const root = connectionRemoteRoot(selected);
    if (!root) {
      setStatus("先在连接里填远程目录");
      return;
    }
    void loadRemoteDir(remoteProject || root);
  }

  function enterRemoteDir(name: string) {
    if (!selected || !remoteBrowser || remoteBrowser.reading) return;
    const next = joinChild(connectionRemoteRoot(selected), remoteBrowser.path, name);
    if (!next) {
      setRemoteBrowser({ ...remoteBrowser, reading: false, error: "不能走出远程目录" });
      return;
    }
    void loadRemoteDir(next);
  }

  function chooseRemoteDir() {
    if (!selected || !remoteBrowser || remoteBrowser.reading || remoteBrowser.error) return;
    const root = connectionRemoteRoot(selected);
    const allowed = browseCrumbs(root, remoteBrowser.path);
    const path = allowed[allowed.length - 1]?.path;
    if (!path) {
      setRemoteBrowser({ ...remoteBrowser, error: "不能走出远程目录" });
      return;
    }
    setRemoteProject(path);
    noteRemoteRoot(path);
    setRemoteBrowser(null);
  }

  return (
    <div className="desktop">
      <div className="window">
        <header className="titlebar">
          <span className="lights" aria-hidden>
            <i /><i /><i />
          </span>
          <div className="titleblock">
            <strong>{page === "ssh" ? "SSH 连接" : page === "history" ? "历史记录" : projectRoot ? baseName(projectRoot) : "合天下"}</strong>
            <span>{page === "ssh" ? `${connections.length} 个连接` : ""}</span>
          </div>
          <nav className="pages">
            <button className={page === "edit" ? "on" : ""} onClick={() => setPage("edit")}>编辑</button>
            <button className={page === "ssh" ? "on" : ""} onClick={() => setPage("ssh")}>连接</button>
            <button className={page === "preview" ? "on" : ""} onClick={() => setPage("preview")}>预演</button>
            <button className={page === "history" ? "on" : ""} onClick={() => setPage("history")}>历史</button>
          </nav>
          {page === "ssh" && (
            <>
              <button className="ghost import" type="button" disabled>从 ~/.ssh/config 导入</button>
              <span className="soon">下一版支持</span>
            </>
          )}
        </header>
        {status && !sheet && <p className={status.includes("E_") || status.startsWith("写到第") ? "status err" : "status"} role="status">{status}</p>}
        {page === "edit" && (
          <DiffEditor
            projectRoot={plan ? projectRoot : null}
            packagePath={plan ? packagePath : null}
            currentFile={currentFile}
            onExecute={(edits) => { void backupAndExecute(edits); }}
          />
        )}
        {page === "ssh" && (
          <SshPage
            connections={connections}
            selectedId={selectedId}
            onPick={setSelectedId}
            onRefresh={setConnections}
            onStatus={setStatus}
          />
        )}
        {page === "preview" && (
          <PreviewPage
            allowReadonly={allowReadonly}
            setAllowReadonly={setAllowReadonly}
            bakDir={bakDir}
            projectRoot={projectRoot}
            packagePath={packagePath}
            plan={plan}
            picked={pickedPaths}
            onPicked={applyPicked}
            scanning={scanning}
            running={running}
            report={report}
            remoteBatch={remoteBatch}
            onPickProject={() => { void onPickProject(); }}
            onPickDirectory={() => { void onPickPackage(pickUpgradeDirectory); }}
            onPickZip={() => { void onPickPackage(pickUpgradeZip); }}
            onExecute={() => { void backupAndExecute(editedContents()); }}
            onOpenFile={(path) => { setCurrentFile(path); setPage("edit"); }}
            connectionSelected={selected != null}
            remoteRootReady={isAbsoluteRemoteRoot(connectionRemoteRoot(selected))}
            remoteRootIsSlash={isFilesystemRemoteRoot(selected?.remoteRoot ?? "")}
            remoteProject={remoteProject}
            onBrowseRemote={openRemoteBrowser}
            onRemoteWrite={() => {
              const pickedFiles = remotePlanFiles(plan, pickedPaths);
              if (!selected) {
                setStatus("先保存并选择一个连接");
                return;
              }
              if (pickedFiles.error) {
                setStatus(pickedFiles.error);
                return;
              }
              if (pickedFiles.files.length === 0) {
                setStatus("还没有选中的计划文件");
                return;
              }
              void remoteWritePrompt(selected.id, pickedFiles.files).then((prompt) => {
                setRemoteTx({
                  execId: prompt.execId,
                  backupDir: prompt.backupDir,
                  title: prompt.title,
                  body: prompt.body,
                  cancelLabel: prompt.cancelLabel,
                  proceedLabel: prompt.proceedLabel,
                  defaultButton: prompt.defaultButton,
                });
              }).catch((err) => setStatus(formatEngineError(err)));
            }}
          />
        )}
        {page === "history" && (
          <HistoryPage
            refreshToken={historyToken}
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
        {pendingExec && (
          <ExecuteConfirmSheet
            preview={pendingExec.preview}
            onCancel={() => dismissExecute("cancel")}
            onEscape={() => dismissExecute("escape")}
            onConfirm={acceptExecute}
          />
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
        {remoteBrowser && (
          <RemoteDirSheet
            root={connectionRemoteRoot(selected)}
            browser={remoteBrowser}
            onCrumb={(path) => { void loadRemoteDir(path); }}
            onEnter={enterRemoteDir}
            onRetry={() => { void loadRemoteDir(remoteBrowser.path); }}
            onCancel={() => setRemoteBrowser(null)}
            onChoose={chooseRemoteDir}
          />
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
                void commitRemoteOverwrite(pending.execId).then((report) => {
                  setRemoteBatch(report);
                  const failure = remoteBatchFailure(report);
                  setStatus(failure ?? `已写入 ${report.total} 个文件`);
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

function RemoteDirSheet({
  root,
  browser,
  onCrumb,
  onEnter,
  onRetry,
  onCancel,
  onChoose,
}: {
  root: string;
  browser: { path: string; dirs: string[]; reading: boolean; error: string | null };
  onCrumb: (path: string) => void;
  onEnter: (name: string) => void;
  onRetry: () => void;
  onCancel: () => void;
  onChoose: () => void;
}) {
  const crumbs = browseCrumbs(root, browser.path);
  return (
    <div className="mask" role="dialog" aria-modal="true" aria-labelledby="remote-dir-title">
      <div className="sheet small">
        <h1 id="remote-dir-title">选择远程项目目录</h1>
        <p className="sub">
          {crumbs.map((crumb, index) => (
            <button type="button" key={crumb.path} onClick={() => onCrumb(crumb.path)}>
              {index === 0 ? crumb.path : crumb.name}
            </button>
          ))}
        </p>
        {browser.reading && <p role="status">正在读取…</p>}
        {browser.error && (
          <p className="err" role="alert">
            读不到这个目录：<Text value={browser.error} />
            <button type="button" onClick={onRetry}>重试</button>
          </p>
        )}
        {!browser.reading && !browser.error && (
          <ul className="changes">
            {browser.dirs.map((name) => (
              <li key={name}>
                <button type="button" onDoubleClick={() => onEnter(name)}><Text value={name} /></button>
              </li>
            ))}
          </ul>
        )}
        <footer>
          <button type="button" onClick={onCancel}>取消</button>
          <span style={{ flex: 1 }} />
          <button type="button" className="primary" onClick={onChoose} disabled={browser.reading || browser.error != null}>选这个目录</button>
        </footer>
      </div>
    </div>
  );
}

function SshPage({
  connections,
  selectedId,
  onPick,
  onRefresh,
  onStatus,
}: {
  connections: SavedConnection[];
  selectedId: string | null;
  onPick: (id: string | null) => void;
  onRefresh: (rows: SavedConnection[]) => void;
  onStatus: (message: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<SavedConnection>(blankConnection());
  const [secretDraft, setSecretDraft] = useState("");
  const [secretSaved, setSecretSaved] = useState(false);
  const [pendingFingerprint, setPendingFingerprint] = useState<string | null>(null);
  const [probe, setProbe] = useState<ProbeReport | null>(null);
  const shown = connections.filter((item) => {
    const text = `${item.name} ${item.user} ${item.host}`.toLowerCase();
    return text.includes(query.trim().toLowerCase());
  });

  function load(id: string | null) {
    onPick(id);
    setSecretDraft("");
    setPendingFingerprint(null);
    setProbe(null);
    const found = connections.find((item) => item.id === id);
    setDraft(found ?? blankConnection());
    if (!id) {
      setSecretSaved(false);
      return;
    }
    void sshSecretStatus(id).then((status) => setSecretSaved(status.saved)).catch(() => setSecretSaved(false));
  }

  async function refresh(id: string | null) {
    const rows = await listConnections();
    onRefresh(rows);
    onPick(id);
    const found = rows.find((item) => item.id === id);
    if (found) setDraft(found);
    if (id) {
      const status = await sshSecretStatus(id);
      setSecretSaved(status.saved);
    }
  }

  async function save() {
    try {
      const saved = await saveConnection(draft);
      if (saved.auth === "agent") {
        await deleteSshSecret(saved.id);
        setSecretDraft("");
      } else if (secretDraft) {
        await saveSshSecret(saved.id, secretDraft);
        setSecretDraft("");
      }
      await refresh(saved.id);
      onStatus("连接已保存");
    } catch (err) {
      onStatus(formatEngineError(err));
    }
  }

  async function test(acceptNew: boolean) {
    if (!draft.id) {
      onStatus("先保存连接");
      return;
    }
    try {
      const report = await testConnection(draft.id, acceptNew, acceptNew ? pendingFingerprint ?? undefined : undefined);
      setProbe(report);
      if (report.hostKey === "new" && !report.authOk) {
        setPendingFingerprint(report.fingerprint);
        onStatus("需要确认主机指纹");
        return;
      }
      setPendingFingerprint(null);
      await refresh(draft.id);
      onStatus(report.authOk ? "连接测试通过" : "连接测试没有完成");
    } catch (err) {
      onStatus(formatEngineError(err));
    }
  }

  async function clearFingerprint() {
    if (!draft.id) return;
    try {
      await clearConnectionFingerprint(draft.id);
      setPendingFingerprint(null);
      await refresh(draft.id);
    } catch (err) {
      onStatus(formatEngineError(err));
    }
  }

  async function remove() {
    if (!draft.id) {
      load(null);
      return;
    }
    try {
      await deleteConnection(draft.id);
      await deleteSshSecret(draft.id);
      const rows = await listConnections();
      onRefresh(rows);
      load(null);
    } catch (err) {
      onStatus(formatEngineError(err));
    }
  }

  const remoteRootError = remoteRootFieldError(draft.remoteRoot);

  return (
    <div className="ssh">
      <aside>
        <input className="search" placeholder="搜索连接" value={query} onChange={(event) => setQuery(event.target.value)} />
        <button type="button" className="ghost" onClick={() => load(null)}>新建连接</button>
        {shown.map((item) => (
          <button key={item.id} type="button" className={`host ${item.id === selectedId ? "sel" : ""}`} onClick={() => load(item.id)}>
            <Server size={16} />
            <span>
              <b><Text value={item.name} /></b>
              <em><Text value={`${item.user}@${item.host}:${item.port}`} /></em>
            </span>
          </button>
        ))}
        {shown.length === 0 && <p>还没有连接</p>}
      </aside>
      <section>
        <div className="hosthead">
          <Server size={18} />
          <div>
            <h1>{draft.name || "新建连接"}</h1>
            <p>{draft.host ? <Text value={`${draft.user}@${draft.host}:${draft.port}`} /> : "填写主机后再测试"}</p>
          </div>
          <button className="ghost" type="button" onClick={() => void test(false)} disabled={!draft.id}>测试连接</button>
        </div>
        <label>名称<input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></label>
        <label>主机<input value={draft.host} onChange={(event) => setDraft({ ...draft, host: event.target.value })} /></label>
        <div className="row2">
          <label>端口<input value={draft.port} onChange={(event) => setDraft({ ...draft, port: Number(event.target.value) || 0 })} /></label>
          <label>用户名<input value={draft.user} onChange={(event) => setDraft({ ...draft, user: event.target.value })} /></label>
        </div>
        <label>远程目录（绝对路径）<span className="req" aria-hidden="true">*</span><input value={draft.remoteRoot ?? ""} placeholder="/srv/app" aria-required="true" aria-invalid={remoteRootError !== null} onChange={(event) => setDraft({ ...draft, remoteRoot: event.target.value === "" ? null : event.target.value })} /></label>
        {remoteRootError && <p className="warn fielderr">{remoteRootError}</p>}
        <div className="auth">认证方式
          <span className={draft.auth === "password" ? "on" : ""} onClick={() => setDraft({ ...draft, auth: "password" })}>密码</span>
          <span className={draft.auth === "key" ? "on" : ""} onClick={() => setDraft({ ...draft, auth: "key" })}>私钥文件</span>
          <span className="off">ssh-agent</span>
          <span className="soon">下一版支持</span>
        </div>
        <label>私钥文件<input value={draft.keyPath} onChange={(event) => setDraft({ ...draft, keyPath: event.target.value })} disabled={draft.auth !== "key"} /></label>
        <label>私钥口令或密码<input type="password" value={draft.auth === "agent" ? "" : secretDraft} autoComplete="new-password" disabled={draft.auth === "agent"} placeholder={draft.auth === "agent" ? "ssh-agent 不保存口令" : secretSaved ? "已存入钥匙串，留空则不改" : "只写入钥匙串"} onChange={(event) => setSecretDraft(event.target.value)} /></label>
        <div className="savekey"><span className={draft.auth !== "agent" && secretSaved ? "ok" : ""}>{draft.auth === "agent" ? "ssh-agent 不保存口令" : secretSaved ? "已存入钥匙串" : "还没存入口令"}</span></div>
        <p className="keyhint">口令只存在本机钥匙串，不会再显示出来</p>
        <h2>连接测试</h2>
        <ul className="checks">
          <li><span>身份认证</span><em>{probe ? (probe.authOk ? "通过" : "未完成") : "尚未测试"}</em></li>
          <li><span>SFTP 子系统</span><em>{probe ? (probe.sftpOk ? "可用" : "不可用") : "尚未测试"}</em></li>
          <li><span>服务器哈希加速</span><em>{probe ? (probe.sha256sumOk ? "sha256sum 可用" : "不可用") : "尚未测试"}</em></li>
        </ul>
        <h2>主机指纹</h2>
        <div className="fp">
          <div>
            <b>{draft.fingerprint ? <Text value={draft.fingerprint} /> : "尚未记住"}</b>
            <em>{pendingFingerprint ? "服务器出示了新指纹，确认后才会记住" : draft.fingerprint ? "已记住" : "测试连接后确认"}</em>
          </div>
          <button className="link" type="button" onClick={() => void clearFingerprint()} disabled={!draft.id || !draft.fingerprint}>清除指纹</button>
        </div>
        {pendingFingerprint && (
          <div className="banner">
            <AlertTriangle size={16} /> 确认指纹 <Text value={pendingFingerprint} />
            <button type="button" className="primary" onClick={() => void test(true)}>确认这个指纹</button>
          </div>
        )}
        <footer>
          <button className="danger" type="button" onClick={() => void remove()} disabled={!draft.id}>删除连接</button>
          <span />
          <button className="ghost" type="button" onClick={() => load(selectedId)}>取消</button>
          <button className="primary" type="button" onClick={() => void save()} disabled={remoteRootError !== null}>保存</button>
        </footer>
      </section>
    </div>
  );
}

function PreviewPage({
  allowReadonly,
  setAllowReadonly,
  bakDir,
  projectRoot,
  packagePath,
  plan,
  picked,
  onPicked,
  scanning,
  running,
  report,
  remoteBatch,
  onPickProject,
  onPickDirectory,
  onPickZip,
  onExecute,
  onOpenFile,
  connectionSelected,
  remoteRootReady,
  remoteRootIsSlash,
  remoteProject,
  onBrowseRemote,
  onRemoteWrite,
}: {
  allowReadonly: boolean;
  setAllowReadonly: (v: boolean) => void;
  bakDir: string | null;
  projectRoot: string | null;
  packagePath: string | null;
  plan: LocalPlan | null;
  picked: ReadonlySet<string>;
  onPicked: (next: ReadonlySet<string>) => void;
  scanning: boolean;
  running: boolean;
  report: ExecReport | null;
  remoteBatch: RemoteBatchReport | null;
  onPickProject: () => void;
  onPickDirectory: () => void;
  onPickZip: () => void;
  onExecute: () => void;
  onOpenFile: (path: string) => void;
  connectionSelected: boolean;
  remoteRootReady: boolean;
  remoteRootIsSlash: boolean;
  remoteProject: string | null;
  onBrowseRemote: () => void;
  onRemoteWrite: () => void;
}) {
  const [target, setTarget] = useState<"local" | "remote">("local");
  const counts = useMemo(() => previewCounts(plan ? selectedPlan(plan, picked).files : [], allowReadonly), [plan, picked, allowReadonly]);
  const summary = selectionSummary(plan?.files ?? [], picked);
  const allPaths = (plan?.files ?? []).map((file) => file.path.replace(/\\/g, "/"));
  const allState = checkState(allPaths, picked);
  const localReady = Boolean(plan) && !scanning && !running;
  const remoteReady = connectionSelected && remoteRootReady && currentRemotePlan() != null;
  const localSub = projectRoot && packagePath ? `${baseName(projectRoot)} · ${projectRoot} ← ${baseName(packagePath)}` : "";
  const readonlyFiles = (plan?.files ?? []).filter((file) => file.readonly && (file.kind === "overwrite" || file.kind === "delete"));
  return (
    <div className="sheetwrap">
      <div className="sheet">
        <h1><ShieldAlert size={18} /> 确认执行升级</h1>
        {!plan && <p>先选项目目录和升级包</p>}
        <p className="sub">{target === "remote" ? (remoteProject || "远程") : localSub}</p>
        <div className="pickrow">
          <button type="button" onClick={target === "remote" ? onBrowseRemote : onPickProject}>{target === "remote" ? "选择远程项目目录…" : "选择项目目录…"}</button>
          <button type="button" onClick={onPickDirectory}>选择升级包文件夹…</button>
          <button type="button" onClick={onPickZip}>选择 zip 升级包…</button>
        </div>
        {scanning && <p role="status">正在扫描…</p>}
        <div className="auth">写入到 <span className={target === "local" ? "on" : ""} onClick={() => setTarget("local")}>本机</span><span className={target === "remote" ? "on" : ""} onClick={() => setTarget("remote")}>远程</span></div>
        {plan && (
          <div className="stats">
            <b>{counts.create}<small>新增</small></b>
            <b>{counts.overwrite}<small>覆盖{counts.merged ? ` · 其中合并 ${counts.merged}` : ""}</small></b>
            <b>{counts.move}<small>移动</small></b>
            <b>{counts.delete}<small>删除</small></b>
            {counts.skipReadonly > 0 && <b className="skip">{counts.skipReadonly}<small>跳过（只读）</small></b>}
          </div>
        )}
        {plan && (
          <div className="pickrow">
            <input
              type="checkbox"
              aria-label="全选"
              checked={allState === "all"}
              ref={(el) => { if (el) el.indeterminate = allState === "some"; }}
              onChange={() => onPicked(applyCheck(picked, allPaths, allState))}
            />
            <span>已选 {summary.selected} / 共 {summary.total} 个文件</span>
            <button type="button" onClick={() => onPicked(invertPicked(allPaths, picked))}>反选</button>
          </div>
        )}
        {plan && (
          <ul className="changes">
            {planListRows(plan.files).map((row) => {
              if (row.kind === "dir") {
                const paths = filePathsUnder(row.path, plan.files);
                const state: CheckState = checkState(paths, picked);
                return (
                  <li key={`dir:${row.path}`} style={row.depth ? { paddingLeft: 10 + row.depth * 16 } : undefined}>
                    <input
                      type="checkbox"
                      checked={state === "all"}
                      ref={(el) => { if (el) el.indeterminate = state === "some"; }}
                      onChange={() => onPicked(applyCheck(picked, paths, state))}
                    />
                    <span><Text value={row.path} /></span>
                    <em>目录</em>
                  </li>
                );
              }
              const file = plan.files.find((item) => item.path.replace(/\\/g, "/") === row.path);
              if (!file) return null;
              const state: CheckState = picked.has(row.path) ? "all" : "none";
              return (
                <li key={file.path} data-keep-source={keepSource(file.label) ? "1" : "0"} style={row.depth ? { paddingLeft: 10 + row.depth * 16 } : undefined}>
                  <input
                    type="checkbox"
                    checked={picked.has(row.path)}
                    onChange={() => onPicked(applyCheck(picked, [row.path], state))}
                  />
                  <i className={file.kind === "create" ? "add" : file.kind === "move" ? "move" : "mod"} />
                  <button type="button" onClick={() => onOpenFile(file.path)}>
                    <Text value={file.kind === "move" && file.from ? `${file.from} → ${file.path}` : file.path} />
                  </button>
                  <em>{file.label}</em>
                </li>
              );
            })}
          </ul>
        )}
        {readonlyFiles.length > 0 && (
          <div className="ro">
            <b>只读文件（{readonlyFiles.length}）</b>
            {readonlyFiles.map((file) => (
              <div key={file.path}><Text value={file.path} /> <em>E_PERMISSION · {allowReadonly ? "将临时去掉只读" : "不勾选将跳过"}</em></div>
            ))}
            <label><input type="checkbox" checked={allowReadonly} onChange={(e) => setAllowReadonly(e.target.checked)} /> 覆盖只读文件</label>
            <p>临时去掉只读属性，写入后恢复。</p>
          </div>
        )}
        {report && (
          <ul className="changes">
            {report.files.map((file) => (
              <li key={`${file.index}-${file.path}`}>
                <span><Text value={file.path} /></span>
                <em>{formatFileResult(file)}</em>
              </li>
            ))}
          </ul>
        )}
        {remoteBatch && (
          <ul className="changes">
            {remoteBatch.files.map((file) => (
              <li key={file.remotePath}>
                <span><Text value={file.remotePath} /></span>
                <em>{remoteFileMark(file)}</em>
              </li>
            ))}
          </ul>
        )}
        {running && <p role="status">正在备份并执行…</p>}
        <p className="bak">执行前备份到 {bakDir ? <Text value={bakDir} /> : "…"}</p>
        {target === "remote" && remoteRootIsSlash && <p>远程目录不能是根目录 /</p>}
        {target === "remote" && !remoteRootReady && !remoteRootIsSlash && <p>先在连接里填远程目录</p>}
        {target === "local" && plan && summary.selected === 0 && <p>至少选一个文件</p>}
        <footer>
          <button className="primary" disabled={target === "local" ? !localReady || summary.selected === 0 : !remoteReady} onClick={() => { if (target === "remote") onRemoteWrite(); else onExecute(); }}>{target === "remote" ? "写入远程" : "备份并执行"}</button>
        </footer>
      </div>
    </div>
  );
}

function ExecuteConfirmSheet({
  preview,
  onCancel,
  onEscape,
  onConfirm,
}: {
  preview: ExecuteConfirm;
  onCancel: () => void;
  onEscape: () => void;
  onConfirm: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      onEscape();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onEscape]);
  return (
    <div className="mask" role="dialog" aria-modal="true" aria-labelledby="confirm-execute-title" onClick={onCancel}>
      <div className="sheet small" onClick={(event) => event.stopPropagation()}>
        <h1 id="confirm-execute-title"><ShieldAlert size={18} /> 确认执行升级</h1>
        <p><Text value={confirmExecuteBody(preview)} /></p>
        {preview.conflicts.length > 0 && (
          <ul className="changes">
            {preview.conflicts.map((file) => (
              <li key={file.path}>
                <span><Text value={file.detail} /></span>
              </li>
            ))}
          </ul>
        )}
        <footer>
          <button ref={cancelRef} type="button" className="ghost" autoFocus onClick={onCancel}>取消</button>
          <button type="button" className="primary" onClick={onConfirm}>确认执行</button>
        </footer>
      </div>
    </div>
  );
}

function HistoryPage({ refreshToken, onRollback, onContinue }: { refreshToken: number; onRollback: (execId: string) => void; onContinue: (action: HistoryContinue) => void }) {
  const [runs, setRuns] = useState<JournalRun[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  useEffect(() => {
    void journalRuns()
      .then((rows) => {
        setRuns(rows);
        setLoadError(null);
      })
      .catch((err) => setLoadError(formatEngineError(err)));
  }, [refreshToken]);
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
