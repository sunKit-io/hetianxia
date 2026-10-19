import { useMemo, useState } from "react";
import { Server, ShieldAlert, KeyRound, AlertTriangle } from "lucide-react";
import { ALPINE, UNKNOWN, UBUNTU, canContinue, connectionSubtitle, systemTypeLabel, warningBanner, type RemoteOs } from "./os";

type Page = "ssh" | "preview" | "history";

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

const HISTORY = [
  { time: "10月12日 14:20", where: "~/work/order-center ← release-3.0.0.zip", change: "新增 6 · 覆盖 13 · 移动 1", result: "成功", rollback: true },
  { time: "9月28日 09:41", where: "~/work/order-center ← release-2.3.1.zip", change: "新增 2 · 覆盖 8", result: "成功", rollback: true },
  { time: "9月03日 18:05", where: "prod-gw-01:/opt/gateway ← gateway-1.8", change: "覆盖 21 · 删除 3", result: "已回滚", rollback: false, cleaned: false },
  { time: "8月11日 10:12", where: "~/work/order-center ← release-2.2.0.zip", change: "新增 4 · 覆盖 17", result: "成功", rollback: false, cleaned: true },
];

export function App() {
  const [page, setPage] = useState<Page>("ssh");
  const [hostId, setHostId] = useState("build-01");
  const [allowReadonly, setAllowReadonly] = useState(false);
  const [sheet, setSheet] = useState<null | "rollback" | "log">(null);
  const [cleared, setCleared] = useState<Record<string, boolean>>({});
  const host = HOSTS.find((h) => h.id === hostId) ?? HOSTS[0];

  return (
    <div className="desktop">
      <div className="window">
        <header className="titlebar">
          <span className="lights" aria-hidden>
            <i /><i /><i />
          </span>
          <div className="titleblock">
            <strong>{page === "ssh" ? "SSH 连接" : page === "history" ? "历史记录" : "OrderCenter"}</strong>
            <span>{page === "ssh" ? `${HOSTS.length} 个连接` : page === "history" ? "OrderCenter · 保留最近 10 次 · 超过 30 天自动清理" : "本机 ~/work/order-center ← release-3.0.0.zip"}</span>
          </div>
          <nav className="pages">
            <button className={page === "ssh" ? "on" : ""} onClick={() => setPage("ssh")}>连接</button>
            <button className={page === "preview" ? "on" : ""} onClick={() => setPage("preview")}>预演</button>
            <button className={page === "history" ? "on" : ""} onClick={() => setPage("history")}>历史</button>
          </nav>
          {page === "ssh" && <button className="ghost import">从 ~/.ssh/config 导入</button>}
        </header>
        {page === "ssh" && (
          <SshPage
            host={host}
            cleared={!!cleared[host.id]}
            onPick={setHostId}
            onClear={() => setCleared((s) => ({ ...s, [host.id]: true }))}
          />
        )}
        {page === "preview" && (
          <PreviewPage allowReadonly={allowReadonly} setAllowReadonly={setAllowReadonly} onLog={() => setSheet("log")} />
        )}
        {page === "history" && <HistoryPage onRollback={() => setSheet("rollback")} />}
        {sheet === "rollback" && <RollbackSheet onClose={() => setSheet(null)} />}
        {sheet === "log" && <LogSheet onClose={() => setSheet(null)} />}
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

function PreviewPage({ allowReadonly, setAllowReadonly, onLog }: { allowReadonly: boolean; setAllowReadonly: (v: boolean) => void; onLog: () => void }) {
  const counts = useMemo(() => {
    const overwrite = allowReadonly ? 13 : 11;
    const skip = allowReadonly ? 0 : 2;
    return { overwrite, skip };
  }, [allowReadonly]);
  return (
    <div className="sheetwrap">
      <div className="sheet">
        <h1><ShieldAlert size={18} /> 确认执行升级</h1>
        <p className="sub">OrderCenter · 本机 ~/work/order-center ← release-3.0.0.zip</p>
        <div className="stats">
          <b>6<small>新增</small></b>
          <b>{counts.overwrite}<small>覆盖{counts.skip ? "" : " · 其中合并 4"}</small></b>
          <b>1<small>移动</small></b>
          <b>0<small>删除</small></b>
          {counts.skip > 0 && <b className="skip">{counts.skip}<small>跳过（只读）</small></b>}
        </div>
        <ul className="changes">
          <li><i className="add" /> plugins/tracing/index.ts <em>升级包</em></li>
          <li><i className="mod" /> src/config/app.config.js <em>合并结果</em></li>
          <li><i className="move" /> lib/format.ts → utils/format.ts <em>移动（保留旧文件）</em></li>
        </ul>
        <div className="ro">
          <b>只读文件（2）</b>
          <div>bin/start.sh <em>E_PERMISSION · {allowReadonly ? "将临时去掉只读" : "不勾选将跳过"}</em></div>
          <div>conf/license.key <em>E_PERMISSION · {allowReadonly ? "将临时去掉只读" : "不勾选将跳过"}</em></div>
          <label><input type="checkbox" checked={allowReadonly} onChange={(e) => setAllowReadonly(e.target.checked)} /> 覆盖只读文件</label>
          <p>临时去掉只读属性，写入后恢复。</p>
        </div>
        <label className="bak"><input type="checkbox" defaultChecked /> 执行前备份到 ~/Library/Application Support/HeTianXia/backups</label>
        <footer>
          <button className="ghost" onClick={onLog}>日志失败示例</button>
          <button className="primary">备份并执行</button>
        </footer>
      </div>
    </div>
  );
}

function HistoryPage({ onRollback }: { onRollback: () => void }) {
  return (
    <div className="history">
      <table>
        <thead><tr><th>时间</th><th>位置</th><th>变更</th><th>结果</th><th /></tr></thead>
        <tbody>
          {HISTORY.map((row) => (
            <tr key={row.time}>
              <td>{row.time}</td>
              <td>{row.where}</td>
              <td>{row.change}</td>
              <td className={row.result === "已回滚" ? "rolled" : "ok"}>{row.result}</td>
              <td>
                {row.cleaned ? <button disabled>备份已清理</button> : row.rollback ? <button onClick={onRollback}>回滚…</button> : <button disabled>回滚…</button>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RollbackSheet({ onClose }: { onClose: () => void }) {
  const [choice, setChoice] = useState("proceed");
  return (
    <div className="mask" onClick={onClose}>
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><AlertTriangle size={18} /> 回滚发现 2 个冲突文件</h1>
        <p>这些文件在 10月12日 升级后又被改动过。</p>
        <ul className="changes">
          <li>src/routes.js <em>升级后修改于 10月14日 09:12</em></li>
          <li>public/index.html <em>升级后修改于 10月13日 17:40</em></li>
        </ul>
        <label className={choice === "proceed" ? "choice on" : "choice"}><input type="radio" checked={choice === "proceed"} onChange={() => setChoice("proceed")} /> 继续回滚（先备份当前版本）<small>先把这 2 个文件的当前版本备份，再覆盖回升级前的样子</small></label>
        <label className={choice === "skip" ? "choice on" : "choice"}><input type="radio" checked={choice === "skip"} onChange={() => setChoice("skip")} /> 跳过冲突的文件<small>其余文件照常回滚，这 2 个保持现状</small></label>
        <footer>
          <button className="ghost" onClick={onClose}>取消</button>
          <button className="primary" onClick={onClose}>继续</button>
        </footer>
      </div>
    </div>
  );
}

function LogSheet({ onClose }: { onClose: () => void }) {
  return (
    <div className="mask" onClick={onClose}>
      <div className="sheet small" onClick={(e) => e.stopPropagation()}>
        <h1><KeyRound size={18} /> 升级已停止：事务日志写入失败</h1>
        <p>可能是磁盘空间不足或没有写权限。已停止写入，不会自动回滚。</p>
        <p>已完成 9 / 20 个文件</p>
        <p className="err">E_LOG_WRITE · No space left on device</p>
        <p>备份清单 manifest.json 仍保留已备份的文件。选「稍后处理」后，下次启动会继续提示这次未完成的升级。</p>
        <footer>
          <button className="ghost" onClick={onClose}>稍后处理</button>
          <button className="primary" onClick={onClose}>回滚</button>
        </footer>
      </div>
    </div>
  );
}
