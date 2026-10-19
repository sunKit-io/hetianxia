import { useState } from "react";
import { AlertTriangle, Check, Server, ShieldCheck } from "lucide-react";
import { HOSTS, type HostStory } from "./demoData";
import { UNKNOWN, canContinue, connectionSubtitle, systemTypeLabel, warningBanner, type RemoteOs } from "./os";

export function SshScreen({ unidentified }: { unidentified: boolean }) {
  const [hosts, setHosts] = useState<HostStory[]>(HOSTS);
  const [hostId, setHostId] = useState("build-01");
  const [cleared, setCleared] = useState<Record<string, boolean>>({});
  const [auth, setAuth] = useState("key");
  const [saveKey, setSaveKey] = useState(true);
  const host = hosts.find((item) => item.id === hostId) ?? hosts[0];
  const os: RemoteOs = unidentified && host.id === "build-01" ? UNKNOWN : host.os;
  const banner = host.proxy ? null : warningBanner(os);
  const usable = canContinue(os) && !host.proxy;
  const subtitle = connectionSubtitle(os);
  const warnSub = os.kind === "unidentified" || (os.kind === "linux" && !os.verified);

  return (
    <div className="ssh">
      <aside>
        <input className="search" placeholder="搜索连接" aria-label="搜索连接" />
        <div className="hostlist">
          {hosts.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`host ${item.id === host.id ? "sel" : ""} ${item.proxy ? "dimhost" : ""}`}
              onClick={() => setHostId(item.id)}
            >
              <Server size={16} />
              <span>
                <b>{item.name}</b>
                <em>{item.proxy ? "需要跳板机，第二版支持" : `${item.user}@${item.host}:${item.port}`}</em>
              </span>
              <i className={`dot ${item.dot}`} />
            </button>
          ))}
        </div>
        <div className="hostadd">
          <button type="button" aria-label="添加连接" onClick={() => addHost(setHosts, setHostId)}>+</button>
          <button type="button" aria-label="删除连接" onClick={() => removeHost(hosts, host.id, setHosts, setHostId)}>−</button>
        </div>
      </aside>
      <section className={host.proxy ? "dim" : ""}>
        <div className="hosthead">
          <Server size={18} />
          <div>
            <h1>{host.name}</h1>
            <p>
              上次连接今天 11:42
              {" · "}
              {os.kind === "linux" && !os.verified ? (
                <>Linux / {os.id || "unknown"} · <span className="warn">未验证的系统</span></>
              ) : (
                <span className={warnSub ? "warn" : ""}>{subtitle}</span>
              )}
            </p>
          </div>
          <button type="button" className="ghost" disabled={!usable}>测试连接</button>
        </div>
        <label className="field">名称<input defaultValue={host.name} disabled={host.proxy} /></label>
        <label className="field">主机<input defaultValue={host.host} disabled={host.proxy} /></label>
        <div className="row2">
          <label className="field">端口<input defaultValue={host.port} disabled={host.proxy} /></label>
          <label className="field">用户名<input defaultValue={host.user} disabled={host.proxy} /></label>
        </div>
        <div className="authlabel">认证方式</div>
        <div className="seg" role="group" aria-label="认证方式">
          <button type="button" className={auth === "password" ? "on" : ""} onClick={() => setAuth("password")} disabled={host.proxy}>密码</button>
          <button type="button" className={auth === "key" ? "on" : ""} onClick={() => setAuth("key")} disabled={host.proxy}>私钥文件</button>
          <button type="button" className={auth === "agent" ? "on" : ""} onClick={() => setAuth("agent")} disabled={host.proxy}>ssh-agent</button>
        </div>
        <label className="field">
          私钥文件
          <span className="filepick">
            <input defaultValue={host.key} disabled={host.proxy} />
            <button type="button" className="ghost" disabled={host.proxy}>选择…</button>
          </span>
        </label>
        <label className="field">
          私钥口令
          <span className="passline">
            <input type="password" defaultValue={host.proxy ? "" : "saved-pass"} disabled={host.proxy} />
            {!host.proxy && <em className="ok">已存入钥匙串</em>}
          </span>
        </label>
        <div className="radios">
          <label><input type="radio" name="keychain" checked={saveKey} onChange={() => setSaveKey(true)} disabled={host.proxy} /> 保存到钥匙串</label>
          <label><input type="radio" name="keychain" checked={!saveKey} onChange={() => setSaveKey(false)} disabled={host.proxy} /> 不保存，每次询问</label>
        </div>
        {banner && (
          <div className="banner"><AlertTriangle size={16} /> <span>{banner}</span></div>
        )}
        {host.proxy && <div className="banner">需要跳板机，第二版支持</div>}
        <h2>连接测试</h2>
        <ul className="checks">
          <li><Check size={14} /><span>TCP 连接</span><em>38 ms</em></li>
          <li><Check size={14} /><span>身份认证</span><em>私钥 ed25519</em></li>
          <li><Check size={14} /><span>SFTP 子系统</span><em>可用</em></li>
          <li><Check size={14} /><span>服务器哈希加速</span><em>sha256sum 可用</em></li>
          <li><Check size={14} /><span>系统类型</span><em>{systemTypeLabel(os)}</em></li>
        </ul>
        <h2>主机指纹</h2>
        <div className="fp">
          <ShieldCheck size={16} />
          <div>
            <b>{cleared[host.id] || !host.fingerprint ? "尚未记住" : host.fingerprint}</b>
            <em>{cleared[host.id] ? "下次连接按首次连接，需要重新确认指纹" : host.seen || "尚未记住"}</em>
          </div>
          <button type="button" className="link" onClick={() => setCleared((s) => ({ ...s, [host.id]: true }))} disabled={host.proxy || !!cleared[host.id] || !host.fingerprint}>清除指纹</button>
        </div>
        <footer>
          <button type="button" className="danger" disabled={host.proxy} onClick={() => removeHost(hosts, host.id, setHosts, setHostId)}>删除连接</button>
          <span className="spacer" />
          <button type="button" className="ghost">取消</button>
          <button type="button" className="primary" disabled={!usable}>保存</button>
        </footer>
      </section>
    </div>
  );
}

function addHost(setHosts: (fn: (prev: HostStory[]) => HostStory[]) => void, setHostId: (id: string) => void) {
  const id = `new-${Date.now()}`;
  const host: HostStory = {
    id,
    name: "新连接",
    user: "deploy",
    host: "",
    port: 22,
    os: UBUNTU_PLACEHOLDER(),
    dot: "off",
    key: "",
    fingerprint: "",
    seen: "",
  };
  setHosts((prev) => [...prev, host]);
  setHostId(id);
}

function UBUNTU_PLACEHOLDER(): RemoteOs {
  return HOSTS[1].os;
}

function removeHost(
  hosts: HostStory[],
  id: string,
  setHosts: (fn: (prev: HostStory[]) => HostStory[]) => void,
  setHostId: (id: string) => void,
) {
  const next = hosts.filter((item) => item.id !== id);
  if (next.length === 0) return;
  setHosts(() => next);
  setHostId(next[0].id);
}
