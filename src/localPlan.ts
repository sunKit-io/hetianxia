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
  leftSize?: number | null;
  rightSize?: number | null;
  leftMtimeMs?: number | null;
  rightMtimeMs?: number | null;
  contentSha256?: string | null;
  entryName?: string | null;
  leftSha256?: string | null;
};

export type PackageSource = {
  scanId: string;
  kind: "dir" | "zip";
  entry: string;
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
  from: string | null;
  expect: { size: number; mtimeMs: number | null; hash: string } | null;
  sourceMtimeMs: number | null;
  keepSource: boolean;
  contentHash: string | null;
  size: number | null;
  source: PackageSource | null;
  /** 远程写入临时带上的内容。本地执行计划不使用，也不会写进计划 JSON。 */
  bytes?: number[];
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
    contentHash: string | null;
    size: number | null;
    source: PackageSource | null;
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
const shownText: Record<string, string> = {};

/** 预演当前选中的文件。远程写入在点击时读这份，所以编辑不用再走一轮渲染。 */
export function noteShownFile(plan: LocalPlan | null, path: string | null) {
  shownPlan = plan;
  shownPath = path;
}

/** 当前打开的那一个文件的文本。计划本身不保存文件字节。 */
export function rememberShownText(path: string, text: string) {
  if (!path) return;
  shownText[path] = text;
}

/** 选中文件的最终文本：有手工编辑用编辑，否则用已经打开的文本。 */
export function shownFilePayload(): { path: string; content: string } | null {
  if (!shownPlan || !shownPath) return null;
  const file = shownPlan.files.find((item) => item.path === shownPath);
  if (!file) return null;
  if (Object.prototype.hasOwnProperty.call(edits, file.path)) {
    return { path: file.path, content: edits[file.path] };
  }
  if (Object.prototype.hasOwnProperty.call(shownText, file.path)) {
    return { path: file.path, content: shownText[file.path] };
  }
  return { path: file.path, content: file.rightText };
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
  for (const key of Object.keys(shownText)) delete shownText[key];
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

/** 和执行器的相对路径规则一致：拒绝 `..`、绝对路径和盘符。 */
export function movePathProblem(raw: string): string | null {
  const text = raw.trim();
  if (!text) return "空路径";
  if (text.startsWith("/") || text.startsWith("~") || text.includes("\\") || text.includes("\0") || text.includes(":")) {
    return `非法路径 ${raw}`;
  }
  const trimmed = text.replace(/^\/+|\/+$/g, "");
  if (!trimmed) return "空路径";
  for (const part of trimmed.split("/")) {
    if (part === "" || part === "." || part === "..") return `路径穿越 ${raw}`;
  }
  return null;
}

function assertMovePaths(path: string, from: string | null) {
  const dest = movePathProblem(path);
  if (dest) throw new Error(dest);
  if (from != null && from !== "") {
    const src = movePathProblem(from);
    if (src) throw new Error(src);
  }
}

function expectOf(file: ScanFile, kind: PlannedFile["kind"]): PlannedFile["expect"] {
  if (kind === "create") return null;
  if (file.leftSize == null || !file.leftSha256) return null;
  return { size: file.leftSize, mtimeMs: file.leftMtimeMs ?? null, hash: file.leftSha256 };
}

/** 只用扫描元数据组计划。不读取、不保存文件字节。 */
export function buildLocalPlan(args: {
  projectRoot: string;
  packagePath: string;
  scanId: string;
  packageKind: "dir" | "zip";
  files: ScanFile[];
}): LocalPlan {
  const planned: PlannedFile[] = [];
  for (const file of args.files) {
    if (file.message === "move-source") continue;
    if (file.status === "same" || file.status === "symlink" || file.status === "error" || file.status === "extra") continue;
    const kind = file.status === "added" ? "create" : file.status === "moved" ? "move" : file.status === "delete" ? "delete" : "overwrite";
    if (kind === "move") assertMovePaths(file.rel, file.movedFrom ?? null);
    const entry = file.entryName || file.rel;
    const source = kind === "delete" || !args.scanId ? null : { scanId: args.scanId, kind: args.packageKind, entry };
    const label = kind === "create" ? "升级包" : kind === "move" ? "移动（保留旧文件）" : kind === "delete" ? "删除" : "覆盖";
    planned.push({
      path: file.rel,
      kind,
      label,
      readonly: Boolean(file.readonly),
      merged: false,
      binary: false,
      leftText: "",
      rightText: "",
      from: kind === "move" ? file.movedFrom ?? null : null,
      expect: expectOf(file, kind),
      sourceMtimeMs: file.rightMtimeMs ?? null,
      keepSource: kind === "move",
      contentHash: kind === "delete" ? null : file.contentSha256 ?? null,
      size: kind === "delete" ? null : file.rightSize ?? null,
      source,
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
  _edits: Record<string, string>,
  _choices: Record<string, MergeDecision>,
  _mergeText: (left: string, right: string, decision: string) => Promise<{ text: string }>,
  id: string,
): Promise<ExecPlan> {
  const ops: ExecPlan["ops"] = [];
  for (const [index, file] of plan.files.entries()) {
    if (file.kind === "move") assertMovePaths(file.path, file.from);
    ops.push({
      index: index + 1,
      kind: file.kind,
      path: file.path,
      from: file.from,
      contentHash: file.kind === "delete" ? null : file.contentHash,
      size: file.kind === "delete" ? null : file.size,
      source: file.kind === "delete" ? null : file.source,
      expect: file.expect,
      mode: null,
      sourceMtimeMs: file.sourceMtimeMs,
      merged: file.merged,
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

