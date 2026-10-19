import { useEffect, useState } from "react";
import { rollbackRun, resumeRun, startupExecId } from "./backend";
import { externalChoice, pendingChoice, rollbackChoice, type ResumeChoice, type RollbackChoice } from "./choices";
import { WindowFrame } from "./chrome";
import {
  CONFLICT_DELETED,
  CONFLICT_MODIFIED,
  PROJECT_SUB,
  PROJECT_TITLE,
  ROLLBACK_FILES,
  ROLLBACK_RECREATED,
  STORY_BACKUP_DIR,
} from "./demoData";
import { EditorScreen, EditorTools } from "./editor";
import { ExternalSheet, PendingSheet, PreviewSheet, RollbackResumeSheet, RollbackSheet, StartupSheet, WriteFailureSheet } from "./dialogs";
import { HistoryScreen, type HistoryVariant } from "./history";
import { SshScreen } from "./ssh";

const SCREENS = [
  "edit",
  "dark",
  "edited",
  "preview",
  "ssh",
  "ssh-unknown",
  "failure",
  "history",
  "resume",
  "conflict",
  "conflict-del",
  "rollback",
  "rollback-recreated",
  "rollback-resume",
  "pending",
  "history-partial",
  "history-attr",
  "history-rolled",
] as const;

type ScreenId = (typeof SCREENS)[number];

function readScreen(): ScreenId {
  const raw = location.hash.replace(/^#/, "");
  return (SCREENS as readonly string[]).includes(raw) ? (raw as ScreenId) : "edit";
}

function historyVariant(screen: ScreenId): HistoryVariant {
  if (screen === "history-partial") return "partial";
  if (screen === "history-attr") return "attr";
  if (screen === "history-rolled") return "rolled";
  return "failure";
}

/**
 * backupDir 由外壳传入，界面只渲染它。
 * 缺省是设计图用的故事数据，方便单独打开预览。
 */
export function App({ backupDir = STORY_BACKUP_DIR }: { backupDir?: string }) {
  const [screen, setScreen] = useState<ScreenId>(readScreen);
  const [query, setQuery] = useState("");
  const [execId, setExecId] = useState("pending-upgrade");
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    const onHash = () => setScreen(readScreen());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  useEffect(() => {
    void startupExecId().then((id) => {
      if (id) setExecId(id);
    }).catch(() => undefined);
  }, []);
  const go = (next: ScreenId) => {
    if (location.hash !== `#${next}`) location.hash = next;
    setScreen(next);
  };
  async function runResume(choice: ResumeChoice, next: ScreenId) {
    try {
      await resumeRun(execId, choice);
      setNotice(null);
      go(next);
    } catch (err) {
      setNotice(err instanceof Error ? err.message : String(err));
    }
  }
  async function runRollback(choice: RollbackChoice, next: ScreenId = "edit") {
    try {
      await rollbackRun(execId, choice);
      setNotice(null);
      go(next);
    } catch (err) {
      setNotice(err instanceof Error ? err.message : String(err));
    }
  }

  const ssh = screen === "ssh" || screen === "ssh-unknown";
  const history = screen === "history" || screen === "history-partial" || screen === "history-attr" || screen === "history-rolled";
  const theme = screen === "dark" ? "dark" : "light";
  const title = ssh ? "SSH 连接" : history ? "历史记录" : PROJECT_TITLE;
  const subtitle = ssh ? "4 个连接" : history ? "OrderCenter · 保留最近 10 次 · 超过 30 天自动清理" : PROJECT_SUB;

  return (
    <WindowFrame
      title={title}
      subtitle={subtitle}
      theme={theme}
      tools={
        ssh ? (
          <button type="button" className="ghost import">从 ~/.ssh/config 导入</button>
        ) : history ? null : (
          <EditorTools query={query} onQuery={setQuery} onHistory={() => go("history")} />
        )
      }
    >
      {notice && <p className="err banner" role="alert">{notice}</p>}
      {!ssh && !history && (
        <EditorScreen
          initialEdited={screen === "edited"}
          query={query}
          onUpgrade={() => go("preview")}
          onOpenHistory={() => go("history")}
          onOpenRollback={() => go("rollback")}
          onOpenSsh={() => go("ssh")}
        />
      )}
      {ssh && <SshScreen unidentified={screen === "ssh-unknown"} />}
      {history && (
        <HistoryScreen
          variant={historyVariant(screen)}
          onRollback={() => void runResume("rollback", "rollback")}
          onContinue={() => void runResume("continue", "conflict")}
          onOpenProject={(id) => go(id === "gateway" ? "ssh" : "edit")}
        />
      )}
      {screen === "preview" && <PreviewSheet backupDir={backupDir} onCancel={() => go("edit")} onRun={() => go("edit")} />}
      {screen === "failure" && <WriteFailureSheet onLater={() => go("edit")} onRollback={() => void runResume("rollback", "rollback")} />}
      {screen === "resume" && (
        <StartupSheet
          onLater={() => void runResume("cancel", "edit")}
          onRollback={() => void runResume("rollback", "rollback")}
          onContinue={() => void runResume("continue", "conflict")}
        />
      )}
      {screen === "conflict" && (
        <ExternalSheet
          files={CONFLICT_MODIFIED}
          onCancel={() => void runResume("cancel", "edit")}
          onKeep={() => void runResume(externalChoice("keep"), "pending")}
          onRollback={() => void runResume(externalChoice("rollback"), "rollback")}
        />
      )}
      {screen === "conflict-del" && (
        <ExternalSheet
          files={CONFLICT_DELETED}
          onCancel={() => void runResume("cancel", "edit")}
          onKeep={() => void runResume(externalChoice("keep"), "pending")}
          onRollback={() => void runResume(externalChoice("rollback"), "rollback")}
        />
      )}
      {screen === "rollback" && (
        <RollbackSheet files={ROLLBACK_FILES} onCancel={() => go("edit")} onApply={(choice) => void runRollback(rollbackChoice(choice))} />
      )}
      {screen === "rollback-recreated" && (
        <RollbackSheet files={ROLLBACK_RECREATED} onCancel={() => go("edit")} onApply={(choice) => void runRollback(rollbackChoice(choice))} />
      )}
      {screen === "pending" && <PendingSheet onCancel={() => go("edit")} onApply={(choice) => void runResume(pendingChoice(choice), "edit")} />}
      {screen === "rollback-resume" && (
        <RollbackResumeSheet onLater={() => void runResume("cancel", "edit")} onContinue={() => void runRollback("proceedBackupCurrent")} />
      )}
    </WindowFrame>
  );
}
