import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { applyCheck, buildLocalPlan, checkState, execPlanFor, filePathsUnder, planListRows, previewCounts, selectedPlan, selectionSummary, textDocToString, textsFromDiff, type ScanFile, type SideText } from "./localPlan.ts";

function side(text: string, extra: Partial<SideText> = {}): SideText {
  const bytes = Array.from(new TextEncoder().encode(text));
  return {
    present: true,
    binary: false,
    text,
    bytes,
    size: bytes.length,
    mtimeMs: 10,
    sha256: `hash:${text}`,
    ...extra,
  };
}

test("text comes from the diff document, not the raw sample", () => {
  const shown = textsFromDiff({
    left: { lines: ["left"], newline: "\n", endsWithNewline: true },
    right: { lines: ["right-a", "right-b"], newline: "\n", endsWithNewline: false },
  });
  assert.equal(shown.left, "left\n");
  assert.equal(shown.right, "right-a\nright-b");
  assert.equal(textDocToString({ lines: [], newline: "\n", endsWithNewline: false }), "");
});

test("scan plus merge builds create, overwrite, and move ops", async () => {
  const files: ScanFile[] = [
    { rel: "new.txt", status: "added" },
    { rel: "keep.txt", status: "same" },
    { rel: "app.txt", status: "modified", readonly: true },
    { rel: "old.txt", status: "moved", message: "move-source", movedTo: "renamed.txt" },
    { rel: "renamed.txt", status: "moved", movedFrom: "old.txt" },
    { rel: "link", status: "symlink" },
    { rel: "only-local.txt", status: "extra" },
  ];
  const reads: string[] = [];
  const plan = await buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    files,
    async read(root, rel) {
      reads.push(`${root}:${rel}`);
      if (rel === "new.txt") return side("new\n");
      if (rel === "app.txt" && root === "/work/demo") return side("old\n");
      if (rel === "app.txt") return side("new-app\n");
      if (rel === "old.txt") return side("same\n", { sha256: "hash:same" });
      return side("same\n");
    },
    async mergeText(left, right) {
      return { text: `M:${left}|${right}` };
    },
  });
  assert.deepEqual(plan.files.map((file) => file.path), ["new.txt", "app.txt", "renamed.txt"]);
  assert.equal(plan.files[0].kind, "create");
  assert.equal(plan.files[0].label, "升级包");
  assert.equal(plan.files[0].merged, true);
  assert.equal(new TextDecoder().decode(new Uint8Array(plan.files[0].bytes)), "M:|new\n");
  assert.equal(plan.files[1].kind, "overwrite");
  assert.equal(plan.files[1].readonly, true);
  assert.equal(plan.files[1].label, "合并结果");
  assert.equal(plan.files[2].kind, "move");
  assert.equal(plan.files[2].from, "old.txt");
  assert.equal(plan.files[2].keepSource, true);
  assert.equal(plan.files[2].merged, false);
  assert.equal(reads.includes("/work/demo:keep.txt"), false);
  const counts = previewCounts(plan.files, false);
  assert.equal(counts.create, 1);
  assert.equal(counts.overwrite, 0);
  assert.equal(counts.skipReadonly, 1);
  assert.equal(counts.move, 1);
  assert.equal(counts.delete, 0);
  const allowed = previewCounts(plan.files, true);
  assert.equal(allowed.overwrite, 1);
  assert.equal(allowed.merged, 2);
  assert.equal(allowed.skipReadonly, 0);
});

test("edited right side is what execute writes", async () => {
  const plan = await buildLocalPlan({
    projectRoot: "/proj",
    packagePath: "/pkg",
    files: [{ rel: "a.txt", status: "modified" }],
    async read(root) {
      return side(root === "/proj" ? "L\n" : "R\n");
    },
    async mergeText(_left, right) {
      return { text: right };
    },
  });
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
  assert.equal(exec.summary.overwrite, 1);
  assert.equal(exec.summary.merged, 1);
});

test("directory checks include nested files and the stored plan keeps one move row", async () => {
  const plan = await buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    files: [
      { rel: "readme.txt", status: "added" },
      { rel: "src/old.ts", status: "moved", message: "move-source", movedTo: "src/new.ts" },
      { rel: "src/new.ts", status: "moved", movedFrom: "src/old.ts" },
      { rel: "src/util/c.ts", status: "added" },
    ],
    async read() {
      return side("same\n");
    },
    async mergeText(_left, right) {
      return { text: right };
    },
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
  const plan = await buildLocalPlan({
    projectRoot: "/work/demo",
    packagePath: "/tmp/pkg",
    files: [
      { rel: "new.txt", status: "added" },
      { rel: "app.txt", status: "modified" },
      { rel: "old.txt", status: "moved", message: "move-source", movedTo: "renamed.txt" },
      { rel: "renamed.txt", status: "moved", movedFrom: "old.txt" },
    ],
    async read() {
      return side("same\n");
    },
    async mergeText(_left, right) {
      return { text: right };
    },
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
