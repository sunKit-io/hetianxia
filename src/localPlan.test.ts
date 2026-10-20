import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { applyCheck, buildLocalPlan, checkState, execPlanFor, filePathsUnder, planListRows, previewCounts, selectedPlan, selectionSummary, textDocToString, textsFromDiff, type ScanFile } from "./localPlan.ts";

test("text comes from the diff document, not the raw sample", () => {
  const shown = textsFromDiff({
    left: { lines: ["left"], newline: "\n", endsWithNewline: true },
    right: { lines: ["right-a", "right-b"], newline: "\n", endsWithNewline: false },
  });
  assert.equal(shown.left, "left\n");
  assert.equal(shown.right, "right-a\nright-b");
  assert.equal(textDocToString({ lines: [], newline: "\n", endsWithNewline: false }), "");
});

function scanned(rel: string, status: string, extra: Partial<ScanFile> = {}): ScanFile {
  return {
    rel,
    status,
    rightSize: 4,
    contentSha256: `sha:${rel}`,
    entryName: rel,
    leftSize: status === "added" ? null : 3,
    leftSha256: status === "added" ? null : `left:${rel}`,
    rightMtimeMs: 20,
    ...extra,
  };
}

test("scan metadata builds create, overwrite, and move ops without file bytes", async () => {
  const files: ScanFile[] = [
    scanned("new.txt", "added"),
    scanned("keep.txt", "same"),
    scanned("app.txt", "modified", { readonly: true }),
    scanned("old.txt", "moved", { message: "move-source", movedTo: "renamed.txt" }),
    scanned("renamed.txt", "moved", { movedFrom: "old.txt", leftSha256: "left:old", leftSize: 3 }),
    scanned("link", "symlink"),
    scanned("only-local.txt", "extra"),
  ];
  const plan = buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    scanId: "scan-1",
    packageKind: "dir",
    files,
  });
  assert.deepEqual(plan.files.map((file) => file.path), ["new.txt", "app.txt", "renamed.txt"]);
  assert.equal(plan.files[0].kind, "create");
  assert.equal(plan.files[0].label, "升级包");
  assert.equal(plan.files[0].merged, false);
  assert.equal(plan.files[0].contentHash, "sha:new.txt");
  assert.equal(plan.files[0].size, 4);
  assert.deepEqual(plan.files[0].source, { scanId: "scan-1", kind: "dir", entry: "new.txt" });
  assert.equal(plan.files[0].expect, null);
  assert.equal(plan.files[1].kind, "overwrite");
  assert.equal(plan.files[1].readonly, true);
  assert.equal(plan.files[1].label, "覆盖");
  assert.equal(plan.files[1].expect?.hash, "left:app.txt");
  assert.equal(plan.files[2].kind, "move");
  assert.equal(plan.files[2].from, "old.txt");
  assert.equal(plan.files[2].keepSource, true);
  assert.equal(plan.files[2].merged, false);
  assert.equal(JSON.stringify(plan).includes('"bytes"'), false);
  const counts = previewCounts(plan.files, false);
  assert.equal(counts.create, 1);
  assert.equal(counts.overwrite, 0);
  assert.equal(counts.skipReadonly, 1);
  assert.equal(counts.move, 1);
  assert.equal(counts.delete, 0);
  const allowed = previewCounts(plan.files, true);
  assert.equal(allowed.overwrite, 1);
  assert.equal(allowed.merged, 0);
  assert.equal(allowed.skipReadonly, 0);
});

test("execute plan keeps the package hash and does not carry bytes", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/proj",
    packagePath: "/pkg.zip",
    scanId: "scan-zip",
    packageKind: "zip",
    files: [scanned("a.txt", "modified", { entryName: "a.txt", contentSha256: "abc", rightSize: 8, rightMtimeMs: 11 })],
  });
  const exec = await execPlanFor(plan, { "a.txt": "edited\n" }, { "a.txt": "takeRight" }, async () => {
    throw new Error("plan must not read or merge file bytes");
  }, "exec-test");
  assert.equal(exec.id, "exec-test");
  assert.equal(exec.projectKey, "/proj");
  assert.equal(exec.ops[0].merged, false);
  assert.equal(exec.ops[0].sourceMtimeMs, 11);
  assert.equal(exec.ops[0].contentHash, "abc");
  assert.equal(exec.ops[0].size, 8);
  assert.deepEqual(exec.ops[0].source, { scanId: "scan-zip", kind: "zip", entry: "a.txt" });
  assert.equal(JSON.stringify(exec).includes('"bytes"'), false);
  assert.equal(exec.summary.overwrite, 1);
  assert.equal(exec.summary.merged, 0);
});

test("directory checks include nested files and the stored plan keeps one move row", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    scanId: "scan-2",
    packageKind: "dir",
    files: [
      scanned("readme.txt", "added"),
      scanned("src/old.ts", "moved", { message: "move-source", movedTo: "src/new.ts" }),
      scanned("src/new.ts", "moved", { movedFrom: "src/old.ts" }),
      scanned("src/util/c.ts", "added"),
    ],
  });
  assert.deepEqual(plan.files.map((file) => file.path), ["readme.txt", "src/new.ts", "src/util/c.ts"]);
  assert.equal(plan.files[1].kind, "move");
  assert.deepEqual(planListRows(plan.files), [
    { kind: "file", path: "readme.txt", depth: 0 },
    { kind: "dir", path: "src", depth: 0 },
    { kind: "file", path: "src/new.ts", depth: 1 },
    { kind: "dir", path: "src/util", depth: 1 },
    { kind: "file", path: "src/util/c.ts", depth: 2 },
  ]);
  const all = plan.files.map((file) => file.path);
  let picked = new Set(all);
  assert.equal(checkState(all, picked), "all");
  assert.deepEqual(selectionSummary(plan.files, picked), { selected: 3, total: 3 });
  const underSrc = filePathsUnder("src", plan.files);
  assert.deepEqual(underSrc, ["src/new.ts", "src/util/c.ts"]);
  picked = applyCheck(picked, underSrc, "all");
  assert.equal(checkState(underSrc, picked), "none");
  assert.equal(checkState(all, picked), "some");
  assert.deepEqual(selectionSummary(plan.files, picked), { selected: 1, total: 3 });
  picked = applyCheck(picked, filePathsUnder("src/util", plan.files), "none");
  assert.equal(checkState(underSrc, picked), "some");
  picked = applyCheck(picked, underSrc, "some");
  assert.equal(checkState(underSrc, picked), "all");
  const onlyMove = selectedPlan(plan, new Set(["src/new.ts"]));
  const exec = await execPlanFor(onlyMove, {}, {}, async () => ({ text: "" }), "pick-1");
  assert.deepEqual(exec.ops.map((op) => op.path), ["src/new.ts"]);
  assert.equal(exec.ops[0].kind, "move");
  assert.equal(exec.ops[0].from, "src/old.ts");
  assert.equal(exec.ops[0].keepSource, true);
  assert.equal(exec.summary.moveCount, 1);
  assert.equal(exec.summary.create, 0);
  assert.equal(exec.summary.overwrite, 0);
});

test("preview header counts follow the checked plan", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    scanId: "scan-header",
    packageKind: "dir",
    files: [
      scanned("new.txt", "added"),
      scanned("app.txt", "modified"),
      scanned("old.txt", "moved", { message: "move-source" }),
      scanned("renamed.txt", "moved", { movedFrom: "old.txt" }),
    ],
  });
  const all = previewCounts(plan.files, false);
  assert.equal(all.create, 1);
  assert.equal(all.overwrite, 1);
  assert.equal(all.move, 1);
  const checked = previewCounts(selectedPlan(plan, new Set(["new.txt", "renamed.txt"])).files, false);
  assert.equal(checked.create, 1);
  assert.equal(checked.overwrite, 0);
  assert.equal(checked.move, 1);
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const preview = app.slice(app.indexOf("function PreviewPage"), app.indexOf("function ExecuteConfirmSheet"));
  assert.match(preview, /previewCounts\(plan \? selectedPlan\(plan, picked\)\.files : \[\]/);
});
