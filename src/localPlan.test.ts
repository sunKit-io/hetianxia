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

test("编辑后的内容就是执行写入的内容", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/proj",
    packagePath: "/pkg.zip",
    scanId: "scan-zip",
    packageKind: "zip",
    files: [
      scanned("a.txt", "modified", { entryName: "a.txt", contentSha256: "abc", rightSize: 8, rightMtimeMs: 11 }),
      scanned("b.txt", "modified", { entryName: "b.txt", contentSha256: "sha:b.txt", rightSize: 4 }),
    ],
  });
  plan.files[0].leftText = "L\n";
  plan.files[0].rightText = "R\n";
  const exec = await execPlanFor(plan, { "a.txt": "edited\n" }, { "a.txt": "takeRight" }, async (left, right, decision) => {
    assert.equal(left, "L\n");
    assert.equal(right, "edited\n");
    assert.equal(decision, "takeRight");
    return { text: "edited\n" };
  }, "exec-test");
  assert.equal(exec.id, "exec-test");
  assert.equal(exec.projectKey, "/proj");
  assert.equal(exec.ops[0].merged, true);
  assert.equal(exec.ops[0].sourceMtimeMs, null);
  assert.equal(new TextDecoder().decode(new Uint8Array(exec.ops[0].bytes ?? [])), "edited\n");
  assert.equal(exec.ops[0].size, "edited\n".length);
  assert.equal(exec.summary.overwrite, 2);
  assert.equal(exec.summary.merged, 1);
  assert.equal(exec.ops[1].bytes, undefined);
  assert.equal(exec.ops[1].merged, false);
  assert.equal(exec.ops[1].contentHash, "sha:b.txt");
  assert.equal(JSON.stringify(exec.ops[1]).includes('"bytes"'), false);
});

test("保留我的 writes the left side into the plan", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/proj",
    packagePath: "/pkg",
    scanId: "scan-left",
    packageKind: "dir",
    files: [scanned("a.txt", "modified")],
  });
  plan.files[0].leftText = "mine\n";
  plan.files[0].rightText = "theirs\n";
  const exec = await execPlanFor(plan, {}, { "a.txt": "keepLeft" }, async (left, right, decision) => {
    assert.equal(left, "mine\n");
    assert.equal(right, "theirs\n");
    assert.equal(decision, "keepLeft");
    return { text: left };
  }, "keep-left");
  assert.equal(exec.ops[0].merged, true);
  assert.equal(exec.ops[0].sourceMtimeMs, null);
  assert.equal(new TextDecoder().decode(new Uint8Array(exec.ops[0].bytes ?? [])), "mine\n");
});

test("两边都要 writes the merged text into the plan", async () => {
  const plan = buildLocalPlan({
    projectRoot: "/proj",
    packagePath: "/pkg",
    scanId: "scan-both",
    packageKind: "dir",
    files: [scanned("a.txt", "modified")],
  });
  plan.files[0].leftText = "L\n";
  plan.files[0].rightText = "R\n";
  const exec = await execPlanFor(plan, {}, { "a.txt": "both" }, async (left, right, decision) => {
    assert.equal(left, "L\n");
    assert.equal(right, "R\n");
    assert.equal(decision, "both");
    return { text: `${left}${right}` };
  }, "keep-both");
  assert.equal(exec.ops[0].merged, true);
  assert.equal(exec.ops[0].sourceMtimeMs, null);
  assert.equal(new TextDecoder().decode(new Uint8Array(exec.ops[0].bytes ?? [])), "L\nR\n");
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

test("move source and destination reject dotdot and absolute paths", async () => {
  const base = {
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    scanId: "scan-move",
    packageKind: "dir" as const,
  };
  for (const rel of ["../escape.txt", "/tmp/escape.txt", "a/../../b.txt"]) {
    assert.throws(() => buildLocalPlan({ ...base, files: [scanned(rel, "moved", { movedFrom: "old.txt" })] }), /路径穿越|非法路径/);
  }
  for (const from of ["../old.txt", "/etc/passwd", "a\\..\\x"]) {
    assert.throws(() => buildLocalPlan({ ...base, files: [scanned("new.txt", "moved", { movedFrom: from })] }), /路径穿越|非法路径/);
  }
  const plan = buildLocalPlan({ ...base, files: [scanned("dir/new.txt", "moved", { movedFrom: "dir/old.txt" })] });
  assert.equal(plan.files[0]?.from, "dir/old.txt");
  plan.files[0]!.from = "../old.txt";
  await assert.rejects(() => execPlanFor(plan, {}, {}, async () => ({ text: "" }), "bad-move"), /路径穿越/);
});
