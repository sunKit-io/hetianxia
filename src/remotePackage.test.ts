import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { clearFileNotes, rememberEdit, type LocalPlan, type PlannedFile } from "./localPlan.ts";
import { noteRemoteRoot, remotePlanFiles } from "./remoteWrite.ts";

function planned(path: string, size: number): PlannedFile {
  return {
    path,
    kind: "overwrite",
    label: "覆盖",
    readonly: false,
    merged: false,
    binary: true,
    leftText: "",
    rightText: "",
    from: null,
    expect: null,
    sourceMtimeMs: null,
    keepSource: false,
    contentHash: "cd".repeat(32),
    size,
    source: { scanId: "scan-pkg", kind: "dir", entry: path },
    bytes: Array.from(new TextEncoder().encode("this text must not be uploaded")),
  };
}

test("unedited package files send path, hash and size, and edits stay as bytes", () => {
  const plan: LocalPlan = {
    projectRoot: "/work/app",
    packagePath: "/tmp/extracted",
    projectKey: "/work/app",
    files: [planned("bin.dat", 5), planned("wide.bin", 5 * 1024 * 1024), planned("gbk.txt", 4)],
  };
  noteRemoteRoot("/srv/site");
  const streamed = remotePlanFiles(plan, new Set(["bin.dat", "wide.bin", "gbk.txt"]));
  assert.equal(streamed.error, null);
  assert.equal(streamed.files.every((file) => file.bytes == null), true);
  assert.deepEqual(
    streamed.files.map((file) => [file.path, file.contentHash, file.size, file.source?.entry]),
    [
      ["bin.dat", "cd".repeat(32), 5, "bin.dat"],
      ["wide.bin", "cd".repeat(32), 5 * 1024 * 1024, "wide.bin"],
      ["gbk.txt", "cd".repeat(32), 4, "gbk.txt"],
    ],
  );
  rememberEdit("gbk.txt", "用户改过\n", "");
  const edited = remotePlanFiles(plan, new Set(["gbk.txt"]));
  assert.deepEqual(edited.files[0]?.bytes, Array.from(new TextEncoder().encode("用户改过\n")));
  assert.equal(edited.files[0]?.source, null);
  const carried = remotePlanFiles(plan, new Set(["bin.dat"]), { "bin.dat": [7, 8] });
  assert.deepEqual(carried.files[0]?.bytes, [7, 8]);
  const missing = remotePlanFiles(
    { ...plan, files: [{ ...plan.files[0], contentHash: null }] },
    new Set(["bin.dat"]),
  );
  assert.match(missing.error ?? "", /哈希或大小/);
  const app = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
  const writeStart = app.indexOf("onRemoteWrite");
  const writeSlice = app.slice(writeStart, app.indexOf('page === "history"', writeStart));
  assert.equal(writeSlice.includes("readSide"), false);
  assert.equal(writeSlice.includes("TextEncoder"), false);
  assert.match(writeSlice, /op\.bytes != null/);
  clearFileNotes();
  noteRemoteRoot("");
});
