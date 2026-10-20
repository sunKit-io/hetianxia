import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
const editor = readFileSync(new URL("./editor.tsx", import.meta.url), "utf8");
const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");

test("preview empty state and execute stay on the real plan", () => {
  assert.match(app, /先选项目目录和升级包/);
  assert.match(app, /选择项目目录…/);
  assert.match(app, /选择升级包文件夹…/);
  assert.match(app, /选择 zip 升级包…/);
  assert.match(app, /onPickPackage\(pickUpgradeDirectory\)/);
  assert.match(app, /onPickPackage\(pickUpgradeZip\)/);
  const directoryPick = backend.slice(backend.indexOf("export function pickUpgradeDirectory"), backend.indexOf("export function pickUpgradeZip"));
  const zipPick = backend.slice(backend.indexOf("export function pickUpgradeZip"));
  assert.match(directoryPick, /directory: true/);
  assert.equal(directoryPick.includes("extensions"), false);
  assert.match(zipPick, /extensions: \["zip"\]/);
  assert.equal(zipPick.includes("directory: true"), false);
  assert.match(directoryPick, /useUpgradePackage/);
  assert.match(zipPick, /useUpgradePackage/);
  assert.match(backend.slice(backend.indexOf("async function useUpgradePackage"), backend.indexOf("export function pickUpgradeDirectory")), /preparePackage/);
  assert.match(app, /备份并执行/);
  assert.match(app, /disabled=\{target === "local" \? !localReady \|\| summary\.selected === 0 : !remoteReady\}/);
  assert.match(app, /已选 \{summary\.selected\} \/ 共 \{summary\.total\} 个文件/);
  assert.match(app, /至少选一个文件/);
  assert.match(app, /aria-label="全选"/);
  assert.match(app, /indeterminate = allState === "some"/);
  assert.match(app, /indeterminate = state === "some"/);
  assert.match(app, /setPickedPaths\(new Set\(next\.files\.map\(\(file\) => file\.path\.replace/);
  assert.match(app, /selectedPlan\(plan, pickedPaths\)/);
  assert.match(app, /execPlanFor\(chosen/);
  assert.match(app, /connectionSelected && remoteRootReady && currentRemotePlan\(\) != null/);
  assert.match(app, /先在连接里填远程目录/);
  assert.match(app, /远程目录不能是根目录 \//);
  assert.match(app, /执行前备份到/);
  assert.equal(app.includes("defaultChecked"), false);
  assert.match(app, /executeLocal\(/);
  assert.match(app, /scanPair\(/);
  assert.match(backend, /"scan"/);
  assert.match(backend, /"diff"/);
  assert.match(backend, /"merge"/);
  assert.match(backend, /"execute"/);
  assert.match(backend, /"read_side"/);
  assert.match(backend, /"prepare_package"/);
  assert.equal(backend.includes("sweep_backups"), false);
  assert.equal(app.includes("sweep_backups"), false);
  const preview = app.slice(app.indexOf("function PreviewPage"), app.indexOf("function HistoryPage"));
  for (const literal of ["OrderCenter", "order-center", "release-3.0.0", "app.config.js", "plugins/tracing", "bin/start.sh"]) {
    assert.equal(preview.includes(literal), false, literal);
  }
});

test("editor uses the shared execute path and real diff text", () => {
  assert.match(editor, /projectRoot/);
  assert.match(editor, /packagePath/);
  assert.match(editor, /currentFile/);
  assert.match(editor, /onExecute/);
  assert.match(editor, /先选项目目录和升级包/);
  assert.match(editor, /loadFileTexts/);
  assert.match(editor, /editedContents\(\)/);
  assert.match(editor, /className="edited">已编辑/);
  assert.match(editor, /disabled=\{!ready\}/);
  assert.equal(editor.includes("onWriteFailed"), false);
  assert.equal(editor.includes("keep1"), false);
  assert.equal(editor.includes("OrderCenter"), false);
  assert.equal(editor.includes("app.config.js"), false);
});

test("title uses the project name only after one is chosen", () => {
  const title = app.slice(app.indexOf("titleblock"), app.indexOf("pages"));
  assert.match(title, /合天下/);
  assert.match(title, /projectRoot \? baseName\(projectRoot\) : "合天下"/);
  assert.equal(title.includes("OrderCenter"), false);
  assert.equal(title.includes("app.config.js"), false);
});
