import assert from "node:assert/strict";
import test from "node:test";
import { DETAIL_DELETED, DETAIL_MODIFIED, resumeConflictTitle } from "./resumeCopy.ts";

test("title switches when any written file was deleted", () => {
  const modified = { path: "src/routes.js", detail: DETAIL_MODIFIED };
  const deleted = { path: "public/old-banner.png", detail: DETAIL_DELETED };
  assert.equal(resumeConflictTitle([modified]), "有 1 个已经写完的文件被改过");
  assert.equal(resumeConflictTitle([modified, deleted]), "有 2 个已经写完的文件被改过或删除");
  assert.equal(DETAIL_MODIFIED, "升级后被修改");
  assert.equal(DETAIL_DELETED, "升级后被删除");
});
