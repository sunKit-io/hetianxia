import { Fragment, useState } from "react";
import { Check, Folder } from "lucide-react";
import {
  HISTORY_ATTR,
  HISTORY_FAILURE,
  HISTORY_PARTIAL,
  HISTORY_ROLLED,
  PROJECTS,
  type HistoryRow,
} from "./demoData";
import { attrRestoredNote, partialRollbackIntro } from "./resumeCopy";

export type HistoryVariant = "failure" | "partial" | "attr" | "rolled";

const TABLES: Record<HistoryVariant, HistoryRow[]> = {
  failure: HISTORY_FAILURE,
  partial: HISTORY_PARTIAL,
  attr: HISTORY_ATTR,
  rolled: HISTORY_ROLLED,
};

export function HistoryScreen({
  variant,
  onRollback,
  onContinue,
  onOpenProject,
}: {
  variant: HistoryVariant;
  onRollback: () => void;
  onContinue: () => void;
  onOpenProject: (id: string) => void;
}) {
  const rows = TABLES[variant];
  const [openUnrestored, setOpenUnrestored] = useState(true);
  const [openAttr, setOpenAttr] = useState(true);
  return (
    <div className="history" data-screen={variant}>
      <aside>
        <p className="sidehead">项目</p>
        {PROJECTS.slice(0, 2).map((project) => (
          <button key={project.id} type="button" className={`siderow ${project.id === "order" ? "soft" : ""}`} onClick={() => onOpenProject(project.id)}>
            <Folder size={15} />
            <span>{project.name}</span>
          </button>
        ))}
        <p className="sidehead">历史记录</p>
        <button type="button" className="siderow soft">
          <span>全部记录</span>
          <b className="count">{rows.length}</b>
        </button>
      </aside>
      <div className="histtable">
        <table>
          <thead>
            <tr><th>时间</th><th>位置</th><th>变更</th><th>结果</th><th /></tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const showUnrestored = !!row.unrestored && openUnrestored;
              const showAttrBody = !!row.attr && openAttr && (row.attrDirect || showUnrestored);
              const showAttrFold = !!row.attr && !openAttr && (row.attrDirect || !!row.unrestored);
              const detail = showUnrestored || showAttrBody || showAttrFold;
              return (
                <Fragment key={row.id}>
                  <tr className={`hrow ${row.tone}`}>
                    <td>{row.time}</td>
                    <td>{row.where}</td>
                    <td>{row.change}</td>
                    <td className={`result ${row.tone}`}>
                      {row.result === "成功" ? <><Check size={14} /> 成功</> : row.result}
                    </td>
                    <td className="actions">
                      {row.actions.includes("unrestored") && (
                        <button type="button" className="ghost tiny" onClick={() => setOpenUnrestored((v) => !v)}>查看未恢复的文件</button>
                      )}
                      {row.actions.includes("rollback") && (
                        <button type="button" className="ghost tiny" onClick={onRollback}>回滚…</button>
                      )}
                      {row.actions.includes("continue") && (
                        <button type="button" className="primary tiny" onClick={onContinue}>继续</button>
                      )}
                      {row.actions.includes("cleaned") && <button type="button" className="quiet" disabled>备份已清理</button>}
                    </td>
                  </tr>
                  {detail && (
                    <tr className="hdetail">
                      <td colSpan={5}>
                        {showUnrestored && row.unrestored && (
                          <div className="detailblock">
                            <p>{partialRollbackIntro(row.unrestored.length)}</p>
                            <ul>
                              {row.unrestored.map((file) => (
                                <li key={file.path}><span>{file.path}</span><em>{file.reason}</em></li>
                              ))}
                            </ul>
                          </div>
                        )}
                        {showAttrBody && row.attr && (
                          <div className="detailblock attr">
                            <p>
                              {attrRestoredNote(row.attr.length)}
                              {" · "}
                              <button type="button" className="textbtn" onClick={() => setOpenAttr(false)}>收起</button>
                            </p>
                            <ul>
                              {row.attr.map((file) => (
                                <li key={file.path}><span>{file.path}</span><em>{file.reason}</em></li>
                              ))}
                            </ul>
                          </div>
                        )}
                        {row.attr && !openAttr && (
                          <div className="detailblock attr">
                            <p>
                              {attrRestoredNote(row.attr.length)}
                              {" · "}
                              <button type="button" className="textbtn" onClick={() => setOpenAttr(true)}>展开</button>
                            </p>
                          </div>
                        )}
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
