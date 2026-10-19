import assert from "node:assert/strict";
import test from "node:test";
import { appDataDir, beginRemoteWrite, settleRemoteWrite, txBackupDir } from "./remoteWrite.ts";

test("dialog names the shell backup directory and cancel writes nothing", () => {
  const execId = "tx-100";
  const macData = appDataDir("/Users/me", "macos");
  assert.equal(macData, "/Users/me/Library/Application Support/com.hetianxia.app");
  const dir = txBackupDir(macData, execId);
  assert.equal(dir, "/Users/me/Library/Application Support/com.hetianxia.app/bak/tx-100");
  const pending = beginRemoteWrite(macData, execId);
  assert.equal(pending.title, "远程写入暂不支持回滚");
  assert.equal(pending.body, `写入前会把远端原文件备份到本机（${dir}），出问题可以从这里手工恢复。`);
  assert.equal(pending.defaultButton, "取消");
  assert.equal(pending.cancelLabel, "取消");
  assert.equal(pending.proceedLabel, "仍然写入");
  assert.equal(pending.body.includes("HeTianXia"), false);
  assert.equal(pending.body.includes("~/"), false);

  const linux = txBackupDir(appDataDir("/home/ubuntu", "linux"), execId);
  assert.equal(linux, "/home/ubuntu/.local/share/com.hetianxia.app/bak/tx-100");
  assert.equal(
    txBackupDir(appDataDir("/home/ubuntu", "linux", "/var/data"), execId),
    "/var/data/com.hetianxia.app/bak/tx-100",
  );

  let writes = 0;
  assert.equal(settleRemoteWrite("取消", () => { writes += 1; }), "cancelled");
  assert.equal(writes, 0);
  assert.equal(settleRemoteWrite("仍然写入", () => { writes += 1; }), "wrote");
  assert.equal(writes, 1);
});
