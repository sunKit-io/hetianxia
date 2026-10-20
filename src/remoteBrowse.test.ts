import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { browseCrumbs, insideRoot, joinChild } from "./remoteBrowse.ts";

test("remote browse starts at the connection root and cannot walk out", () => {
  assert.equal(insideRoot("/srv/site", "/srv/site"), "/srv/site");
  assert.equal(insideRoot("/srv/site/", "/srv/site/app"), "/srv/site/app");
  assert.equal(insideRoot("/srv/site", "/srv/site/../etc"), null);
  assert.equal(insideRoot("/srv/site", "/etc"), null);
  assert.equal(insideRoot("/srv/site", "/srv/site-extra"), null);
  assert.equal(insideRoot("/srv/site", "/"), null);
  assert.equal(joinChild("/srv/site", "/srv/site", "app"), "/srv/site/app");
  assert.equal(joinChild("/srv/site", "/srv/site/app", ".."), null);
  assert.equal(joinChild("/srv/site", "/srv/site", "../etc"), null);
  assert.equal(joinChild("/srv/site", "/srv/site", "a/b"), null);
  assert.deepEqual(browseCrumbs("/srv/site", "/srv/site/app/bin").map((crumb) => crumb.path), [
    "/srv/site",
    "/srv/site/app",
    "/srv/site/app/bin",
  ]);
  assert.equal(browseCrumbs("/srv/site", "/etc").length, 0);
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const backend = readFileSync(new URL("./backend.ts", import.meta.url), "utf8");
  const sheet = app.slice(app.indexOf("function RemoteDirSheet"), app.indexOf("function SshPage"));
  assert.match(app, /选择远程项目目录…/);
  assert.match(app, /target === "remote" \? onBrowseRemote : onPickProject/);
  assert.match(sheet, /取消/);
  assert.match(sheet, /选这个目录/);
  assert.match(sheet, /正在读取…/);
  assert.match(sheet, /读不到这个目录：/);
  assert.match(sheet, /重试/);
  assert.match(sheet, /onDoubleClick/);
  assert.match(app, /joinChild\(/);
  assert.match(sheet, /browseCrumbs\(/);
  assert.equal(sheet.includes("pickProjectDirectory"), false);
  assert.equal(sheet.includes("open("), false);
  assert.match(backend, /"list_remote_dirs"/);
});
