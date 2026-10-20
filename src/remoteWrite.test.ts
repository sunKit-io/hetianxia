import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { clearFileNotes, noteShownFile, rememberEdit, type LocalPlan } from "./localPlan.ts";
import { connectionRemoteRoot, currentRemotePlan, isAbsoluteRemoteRoot, joinRemote, noteRemoteRoot, remoteWriteBody, settleRemoteWrite, verifyFailureBody } from "./remoteWrite.ts";

test("dialog copy uses the runtime backup path and cancel writes nothing", () => {
  const dir = "/var/lib/hetianxia/bak/tx-100";
  assert.equal(remoteWriteBody(dir), `写入前会把远端原文件备份到本机（${dir}），出问题可以从这里手工恢复。`);
  const backup = `${dir}/files/opt/gateway/app.conf`;
  const remote = "/opt/gateway/app.conf";
  assert.equal(
    verifyFailureBody(remote, backup),
    `${remote} 写入后的内容和预期不一致。写入前的原文件已备份到本机（${backup}），可以从这里手工恢复。`,
  );
  let writes = 0;
  assert.equal(settleRemoteWrite("取消", () => { writes += 1; }), "cancelled");
  assert.equal(writes, 0);
  assert.equal(settleRemoteWrite("仍然写入", () => { writes += 1; }), "wrote");
  assert.equal(writes, 1);
});

function filesUnder(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...filesUnder(path));
    else if (/\.(tsx?|css|html)$/.test(name)) out.push(path);
  }
  return out;
}

function samplePlan(text: string): LocalPlan {
  return {
    projectRoot: "/work/app",
    packagePath: "/tmp/pkg",
    projectKey: "/work/app",
    files: [{
      path: "src/app.js",
      kind: "overwrite",
      label: "合并结果",
      readonly: false,
      merged: true,
      binary: false,
      leftText: "old\n",
      rightText: text,
      bytes: Array.from(new TextEncoder().encode(text)),
      from: null,
      expect: null,
      sourceMtimeMs: null,
      keepSource: false,
    }],
  };
}

test("currentRemotePlan joins remoteRoot with the relative path and keeps edits", () => {
  const connection = {
    id: "1",
    name: "edge",
    host: "10.0.0.8",
    port: 22,
    user: "ops",
    auth: "key",
    keyPath: "/home/ops/.ssh/id_ed25519",
    fingerprint: "SHA256:aa",
    remoteRoot: "/srv/site",
  };
  assert.equal(connectionRemoteRoot({ ...connection, remoteRoot: undefined }), "");
  assert.equal(connectionRemoteRoot(connection), "/srv/site");
  assert.equal(connectionRemoteRoot({ ...connection, remoteRoot: "srv/site" }), "srv/site");
  assert.equal(isAbsoluteRemoteRoot(""), false);
  assert.equal(isAbsoluteRemoteRoot("srv/site"), false);
  assert.equal(isAbsoluteRemoteRoot("/srv/site"), true);
  assert.equal(currentRemotePlan(), null);
  noteShownFile(samplePlan("next\n"), "src/app.js");
  noteRemoteRoot("");
  assert.equal(currentRemotePlan(), null);
  noteRemoteRoot("srv/site");
  assert.equal(currentRemotePlan(), null);
  noteRemoteRoot("/srv/site");
  assert.deepEqual(currentRemotePlan(), { remotePath: "/srv/site/src/app.js", content: "next\n" });
  rememberEdit("src/app.js", "edited\n", "next\n");
  assert.equal(currentRemotePlan()?.content, "edited\n");
  assert.equal(currentRemotePlan()?.remotePath, "/srv/site/src/app.js");
  clearFileNotes();
  noteShownFile(null, null);
  noteRemoteRoot("");
});

test("joinRemote stays inside an absolute POSIX root", () => {
  assert.equal(joinRemote("/srv/site/", "src/app.js"), "/srv/site/src/app.js");
  assert.equal(joinRemote("/srv/site", "./src/app.js"), "/srv/site/src/app.js");
  assert.equal(joinRemote("/srv/site", "src/../app.js"), "/srv/site/app.js");
  assert.equal(joinRemote("/srv/site", "../etc/passwd"), "");
  assert.equal(joinRemote("/srv/site", "foo/../../etc/passwd"), "");
  assert.equal(joinRemote("/srv/site", "/etc/passwd"), "");
  assert.equal(joinRemote("/srv", "site-extra/a"), "/srv/site-extra/a");
  assert.equal(joinRemote("/srv/site", "site-extra"), "/srv/site/site-extra");
  assert.equal(joinRemote("srv/site", "a"), "");
  assert.equal(joinRemote("", "src/app.js"), "");
  assert.equal(joinRemote("/srv/../etc", "passwd"), "");
});

test("the frontend has no baked home or product backup path", () => {
  const root = new URL(".", import.meta.url);
  const sources = filesUnder(root.pathname)
    .filter((path) => !path.endsWith(".test.ts"))
    .map((path) => readFileSync(path, "utf8"))
    .join("\n");
  const vite = readFileSync(new URL("../vite.config.ts", import.meta.url), "utf8");
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  assert.equal(sources.includes("HeTianXia/"), false);
  assert.equal(sources.includes("Library/Application"), false);
  assert.equal(sources.includes("previewAppDataDir"), false);
  assert.equal(sources.includes("__HETIANXIA_APP_DATA_DIR__"), false);
  assert.equal(vite.includes("process.env.HOME"), false);
  assert.match(app, /remoteWritePrompt\(/);
  assert.match(app, /commitRemoteOverwrite\(pending\.execId\)/);
  assert.match(app, /新建连接/);
  assert.match(app, /testConnection\(/);
  assert.match(app, /从 ~\/\.ssh\/config 导入/);
  assert.match(app, /disabled>从 ~\/\.ssh\/config 导入/);
  assert.match(app, /ssh-agent<\/span>/);
  assert.match(app, /className="off">ssh-agent/);
  assert.match(app, /下一版支持/);
  assert.equal(app.includes("readSshConfig("), false);
  assert.match(app, /口令只存在本机钥匙串，不会再显示出来/);
  assert.match(app, /远程目录/);
  assert.match(readFileSync(new URL("./backend.ts", import.meta.url), "utf8"), /remoteRoot: string \| null/);
  assert.equal(app.includes("账号是连接 id"), false);
  assert.equal(app.includes("probe_remote"), false);
  assert.equal(app.includes("trust_host_fingerprint"), false);
  assert.equal(app.includes("trustHostFingerprint"), false);
  for (const needle of ["10.0.8.15", "172.16.2.40", "gateway-1.8", "/opt/gateway", "id_ed25519", "HeTianXia"]) {
    assert.equal(sources.includes(needle), false, needle);
  }
  assert.match(app, /appBakDir\(/);
  assert.match(app, /VERIFY_FAILED_TITLE/);
  assert.match(app, /VERIFY_FAILED_BUTTON/);
  assert.match(sources, /远端文件写完后核对不一致/);
  assert.match(sources, /VERIFY_FAILED_BUTTON = "好"/);
});
