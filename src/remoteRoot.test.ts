import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { remoteRootFieldError } from "./remoteRoot.ts";

test("remote directory field rejects a missing path, a relative path, and a normalized root", () => {
  assert.equal(remoteRootFieldError(null), "先在连接里填远程目录");
  assert.equal(remoteRootFieldError(""), "先在连接里填远程目录");
  assert.equal(remoteRootFieldError("   "), "先在连接里填远程目录");
  assert.equal(remoteRootFieldError("srv/app"), "远程目录必须是绝对路径");
  assert.equal(remoteRootFieldError("opt"), "远程目录必须是绝对路径");
  for (const raw of ["/", "//", "/.", "/./", "/srv/..", "/srv/../"]) {
    assert.equal(remoteRootFieldError(raw), "远程目录不能是根目录 /", raw);
  }
  assert.equal(remoteRootFieldError("/srv/app"), null);
  assert.equal(remoteRootFieldError("/opt//./app"), null);
  assert.equal(remoteRootFieldError(" /srv/app/ "), null);
});

test("connection form labels the remote directory as a required absolute path and disables save", () => {
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  assert.match(app, /远程目录（绝对路径）<span className="req" aria-hidden="true">\*<\/span>/);
  assert.match(app, /const remoteRootError = remoteRootFieldError\(draft\.remoteRoot\)/);
  assert.match(app, /\{remoteRootError && <p className="warn fielderr">\{remoteRootError\}<\/p>\}/);
  assert.match(app, /disabled=\{remoteRootError !== null\}>保存/);
});
