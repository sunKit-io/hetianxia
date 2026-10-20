export type SideText = {
  present: boolean;
  binary: boolean;
  text: string;
  bytes: number[];
  size: number;
  mtimeMs: number | null;
  sha256: string;
};

/** 一次本地升级的计划。执行时再套上手工编辑。 */

export type ScanFile = {
  rel: string;
  status: string;
  movedFrom?: string | null;
  message?: string | null;
  readonly?: boolean;
};

export type PlannedFile = {
  path: string;
  kind: "create" | "overwrite" | "move" | "delete";
  label: string;
  readonly: boolean;
  merged: boolean;
  binary: boolean;
  leftText: string;
  rightText: string;
  bytes: number[];
  from: string | null;
  expect: { size: number; mtimeMs: number | null; hash: string } | null;
  sourceMtimeMs: number | null;
  keepSource: boolean;
};

export type LocalPlan = {
  projectRoot: string;
  packagePath: string;
  projectKey: string;
  files: PlannedFile[];
};

export type ExecPlan = {
  id: string;
  projectKey: string;
  ops: Array<{
    index: number;
    kind: PlannedFile["kind"];
    path: string;
    from: string | null;
    bytes: number[] | null;
    expect: PlannedFile["expect"];
    mode: null;
    sourceMtimeMs: number | null;
    merged: boolean;
    keepSource: boolean;
    readonly: boolean;
    label: string;
  }>;
  summary: {
    create: number;
    overwrite: number;
    moveCount: number;
    delete: number;
    merged: number;
    moveKeepSource: number;
  };
};

export type PreviewCounts = {
  create: number;
  overwrite: number;
  move: number;
  delete: number;
  skipReadonly: number;
  merged: number;
};

export type MergeDecision = "keepLeft" | "takeRight" | "both";

export type TextDoc = {
  lines: string[];
  newline: string;
  endsWithNewline: boolean;
};

const edits: Record<string, string> = {};
const decisions: Record<string, MergeDecision> = {};
let statusSink: ((message: string | null) => void) | null = null;
let shownPlan: LocalPlan | null = null;
let shownPath: string | null = null;

/** 预演当前选中的文件。远程写入在点击时读这份，所以编辑不用再走一轮渲染。 */
export function noteShownFile(plan: LocalPlan | null, path: string | null) {
  shownPlan = plan;
  shownPath = path;
}

/** 选中文件的最终文本：有手工编辑用编辑，否则用计划里已经合并好的字节。 */
export function shownFilePayload(): { path: string; content: string } | null {
  if (!shownPlan || !shownPath) return null;
  const file = shownPlan.files.find((item) => item.path === shownPath);
  if (!file) return null;
  if (Object.prototype.hasOwnProperty.call(edits, file.path)) {
    return { path: file.path, content: edits[file.path] };
  }
  return { path: file.path, content: new TextDecoder().decode(Uint8Array.from(file.bytes)) };
}

export function attachFlowStatus(fn: (message: string | null) => void) {
  statusSink = fn;
  return () => {
    if (statusSink === fn) statusSink = null;
  };
}

export function reportFlowStatus(message: string | null) {
  statusSink?.(message);
}

export function clearFileNotes() {
  for (const key of Object.keys(edits)) delete edits[key];
  for (const key of Object.keys(decisions)) delete decisions[key];
}

export function rememberEdit(path: string, text: string, base: string) {
  if (text === base) delete edits[path];
  else edits[path] = text;
}

export function editedContents(): Record<string, string> {
  return { ...edits };
}

export function rememberDecision(path: string, decision: MergeDecision) {
  if (!path) return;
  decisions[path] = decision;
}

export function fileDecision(path: string): MergeDecision {
  return decisions[path] ?? "takeRight";
}

export function decisionMap(): Record<string, MergeDecision> {
  return { ...decisions };
}

export function baseName(path: string): string {
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

export function textDocToString(doc: TextDoc): string {
  const newline = doc.newline || "\n";
  let text = doc.lines.join(newline);
  if (doc.endsWithNewline) text += newline;
  return text;
}

export function textsFromDiff(file: { left: TextDoc; right: TextDoc }): { left: string; right: string } {
  return { left: textDocToString(file.left), right: textDocToString(file.right) };
}

export function newExecId(): string {
  const rand = Math.random().toString(16).slice(2);
  return `local-${Date.now().toString(16)}-${rand}`;
}

export function previewCounts(files: PlannedFile[], allowReadonly: boolean): PreviewCounts {
  const counts: PreviewCounts = { create: 0, overwrite: 0, move: 0, delete: 0, skipReadonly: 0, merged: 0 };
  for (const file of files) {
    if (file.readonly && !allowReadonly && (file.kind === "overwrite" || file.kind === "delete")) {
      counts.skipReadonly += 1;
      continue;
    }
    if (file.kind === "create") counts.create += 1;
    else if (file.kind === "overwrite") counts.overwrite += 1;
    else if (file.kind === "move") counts.move += 1;
    else counts.delete += 1;
    if (file.merged) counts.merged += 1;
  }
  return counts;
}

export type CheckState = "all" | "some" | "none";

export type PlanListRow = { kind: "dir" | "file"; path: string; depth: number };

function slashPath(path: string): string {
  return path.replace(/\\/g, "/");
}

function parentPath(path: string): string {
  const slash = slashPath(path);
  const cut = slash.lastIndexOf("/");
  return cut < 0 ? "" : slash.slice(0, cut);
}

function baseSegment(path: string): string {
  const slash = slashPath(path);
  const cut = slash.lastIndexOf("/");
  return cut < 0 ? slash : slash.slice(cut + 1);
}

/** 目录行在文件行前面按层展开。移动在计划里已经是一条，这里不再拆。 */
export function planListRows(files: readonly { path: string }[]): PlanListRow[] {
  const dirs = new Set<string>();
  const filePaths = files.map((file) => slashPath(file.path));
  for (const path of filePaths) {
    let parent = parentPath(path);
    while (parent) {
      dirs.add(parent);
      parent = parentPath(parent);
    }
  }
  const childDirs = new Map<string, string[]>();
  const childFiles = new Map<string, string[]>();
  const push = (map: Map<string, string[]>, key: string, value: string) => {
    const list = map.get(key);
    if (list) list.push(value);
    else map.set(key, [value]);
  };
  for (const dir of dirs) push(childDirs, parentPath(dir), dir);
  for (const path of filePaths) push(childFiles, parentPath(path), path);
  const rows: PlanListRow[] = [];
  const walk = (dir: string, depth: number) => {
    const entries = [
      ...(childDirs.get(dir) ?? []).map((path) => ({ kind: "dir" as const, path, name: baseSegment(path) })),
      ...(childFiles.get(dir) ?? []).map((path) => ({ kind: "file" as const, path, name: baseSegment(path) })),
    ];
    entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.kind === b.kind ? 0 : a.kind === "dir" ? -1 : 1));
    for (const entry of entries) {
      if (entry.kind === "dir") {
        rows.push({ kind: "dir", path: entry.path, depth });
        walk(entry.path, depth + 1);
      } else {
        rows.push({ kind: "file", path: entry.path, depth });
      }
    }
  };
  walk("", 0);
  return rows;
}

export function filePathsUnder(dir: string, files: readonly { path: string }[]): string[] {
  const prefix = `${slashPath(dir)}/`;
  return files.map((file) => slashPath(file.path)).filter((path) => path.startsWith(prefix));
}

export function checkState(paths: readonly string[], picked: ReadonlySet<string>): CheckState {
  if (paths.length === 0) return "none";
  let hits = 0;
  for (const path of paths) if (picked.has(path)) hits += 1;
  if (hits === 0) return "none";
  if (hits === paths.length) return "all";
  return "some";
}

/** 全选再点变成全不选；半选或未选再点变成全选。 */
export function applyCheck(picked: ReadonlySet<string>, paths: readonly string[], state: CheckState): Set<string> {
  const next = new Set(picked);
  const turnOn = state !== "all";
  for (const path of paths) {
    if (turnOn) next.add(path);
    else next.delete(path);
  }
  return next;
}

export function selectionSummary(files: readonly { path: string }[], picked: ReadonlySet<string>): { selected: number; total: number } {
  const paths = files.map((file) => slashPath(file.path));
  return { selected: paths.filter((path) => picked.has(path)).length, total: paths.length };
}

/** 执行计划只留下勾选的文件。移动仍是一条。 */
export function selectedPlan(plan: LocalPlan, picked: ReadonlySet<string>): LocalPlan {
  return { ...plan, files: plan.files.filter((file) => picked.has(slashPath(file.path))) };
}

export function firstTextFile(plan: LocalPlan): string | null {
  const text = plan.files.find((file) => !file.binary && (file.kind === "overwrite" || file.kind === "create"));
  return text?.path ?? plan.files[0]?.path ?? null;
}

function emptySide(): SideText {
  return { present: false, binary: false, text: "", bytes: [], size: 0, mtimeMs: null, sha256: "" };
}

function snapshot(side: SideText): PlannedFile["expect"] {
  if (!side.present) return null;
  return { size: side.size, mtimeMs: side.mtimeMs, hash: side.sha256 };
}

export async function buildLocalPlan(args: {
  projectRoot: string;
  packagePath: string;
  files: ScanFile[];
  read: (root: string, rel: string) => Promise<SideText>;
  mergeText: (left: string, right: string, decision: string) => Promise<{ text: string }>;
}): Promise<LocalPlan> {
  const planned: PlannedFile[] = [];
  for (const file of args.files) {
    if (file.message === "move-source") continue;
    if (file.status === "same" || file.status === "symlink" || file.status === "error" || file.status === "extra") continue;
    const rightRel = file.rel;
    const leftRel = file.status === "moved" ? file.movedFrom || file.rel : file.rel;
    const left = file.status === "added" ? emptySide() : await args.read(args.projectRoot, leftRel);
    const right = file.status === "delete" ? emptySide() : await args.read(args.packagePath, rightRel);
    const binary = left.binary || right.binary;
    let bytes = right.bytes;
    let merged = false;
    let sourceMtimeMs = right.mtimeMs;
    const textChange = !binary && (file.status === "modified" || file.status === "added");
    if (textChange) {
      const rendered = await args.mergeText(left.text, right.text, "takeRight");
      bytes = Array.from(new TextEncoder().encode(rendered.text));
      merged = true;
      sourceMtimeMs = null;
    }
    const kind = file.status === "added" ? "create" : file.status === "moved" ? "move" : "overwrite";
    const label = kind === "create" ? "升级包" : kind === "move" ? "移动（保留旧文件）" : merged ? "合并结果" : "覆盖";
    planned.push({
      path: file.rel,
      kind,
      label,
      readonly: Boolean(file.readonly),
      merged,
      binary,
      leftText: left.text,
      rightText: right.text,
      bytes,
      from: kind === "move" ? file.movedFrom ?? null : null,
      expect: kind === "create" ? null : snapshot(left),
      sourceMtimeMs,
      keepSource: kind === "move",
    });
  }
  return {
    projectRoot: args.projectRoot,
    packagePath: args.packagePath,
    projectKey: args.projectRoot,
    files: planned,
  };
}

export async function execPlanFor(
  plan: LocalPlan,
  edits: Record<string, string>,
  choices: Record<string, MergeDecision>,
  mergeText: (left: string, right: string, decision: string) => Promise<{ text: string }>,
  id: string,
): Promise<ExecPlan> {
  const ops: ExecPlan["ops"] = [];
  for (const [index, file] of plan.files.entries()) {
    let bytes = file.bytes;
    let merged = file.merged;
    let sourceMtimeMs = file.sourceMtimeMs;
    const decision = choices[file.path] ?? "takeRight";
    const edited = Object.prototype.hasOwnProperty.call(edits, file.path);
    if (!file.binary && (file.kind === "overwrite" || file.kind === "create") && (edited || decision !== "takeRight")) {
      const right = edited ? edits[file.path] : file.rightText;
      const rendered = await mergeText(file.leftText, right, decision);
      bytes = Array.from(new TextEncoder().encode(rendered.text));
      merged = true;
      sourceMtimeMs = null;
    }
    ops.push({
      index: index + 1,
      kind: file.kind,
      path: file.path,
      from: file.from,
      bytes: file.kind === "delete" ? null : bytes,
      expect: file.expect,
      mode: null,
      sourceMtimeMs,
      merged,
      keepSource: file.keepSource,
      readonly: file.readonly,
      label: file.label,
    });
  }
  const summary = { create: 0, overwrite: 0, moveCount: 0, delete: 0, merged: 0, moveKeepSource: 0 };
  for (const op of ops) {
    if (op.kind === "create") summary.create += 1;
    else if (op.kind === "overwrite") summary.overwrite += 1;
    else if (op.kind === "move") summary.moveCount += 1;
    else summary.delete += 1;
    if (op.merged) summary.merged += 1;
    if (op.kind === "move" && op.keepSource) summary.moveKeepSource += 1;
  }
  return { id, projectKey: plan.projectKey, ops, summary };
}

