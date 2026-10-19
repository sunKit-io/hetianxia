import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("merge toolbar labels", () => {
  const editor = readFileSync(new URL("./editor.tsx", import.meta.url), "utf8");
  for (const label of ["用右边", "保留左边", "都保留", "恢复", "全部用右边", "全部保留左边", "全部恢复"]) {
    assert.ok(editor.includes(`>${label}<`), label);
  }
});
