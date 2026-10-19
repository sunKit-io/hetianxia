import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
const editor = readFileSync(new URL("./editor.tsx", import.meta.url), "utf8");
const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");

test("preview empty state and execute stay on the real plan", () => {
  assert.match(app, /先选项目目录和升级包/);
  assert.match(app, /选择项目目录…/);
  assert.match(app, /选择升级包…/);
  assert.match(app, /备份并执行/);
  assert.match(app, /disabled=\{target === "local" \? !localReady : false\}/);
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
  assert.match(editor, /className="edited"/);
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
