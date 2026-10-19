/** 设计图用的故事数据。备份目录只放在这里，界面从 backupDir 原样渲染。 */
import { ALPINE, UBUNTU, type RemoteOs } from "./os";
import {
  ATTR_UNRESTORED,
  PARTIAL_ROLLBACK_CHANGE,
  PARTIAL_ROLLBACK_RESULT,
  PARTIAL_ROLLBACK_UNRESTORED,
  RECREATED_REASON,
  type ResumeFile,
  type UnrestoredFile,
} from "./resumeCopy";

export const STORY_BACKUP_DIR = "~/Library/Application Support/HeTianXia/bak/20261012-1420/";

export const PROJECT_TITLE = "OrderCenter";
export const PROJECT_SUB = "本地 ~/work/order-center ← release-3.0.0.zip";

export const PROJECTS = [
  { id: "order", name: "OrderCenter", badge: "23" },
  { id: "gateway", name: "网关 · prod-gw-01" },
  { id: "admin", name: "管理后台" },
];

export const SCHEME_NAME = "OrderCenter 默认方案";

export const SIDE_HISTORY = [
  { id: "1012", label: "10月12日 14:20", ok: true, rollback: false },
  { id: "0928", label: "9月28日 09:41", ok: true, rollback: false },
  { id: "0903", label: "9月03日 18:05", ok: false, rollback: true },
];

export type Mark = "M" | "A" | "R" | "D";
export type Facet = "add" | "mod" | "extra" | "move";

export type TreeNode = {
  id: string;
  name: string;
  kind: "dir" | "file";
  badge?: string;
  status?: string;
  mark?: Mark;
  checked: boolean;
  filters: Facet[];
  extra?: boolean;
  hint?: string;
  children?: TreeNode[];
};

export const TREE: TreeNode[] = [
  {
    id: "src",
    name: "src",
    kind: "dir",
    badge: "12项",
    checked: true,
    filters: [],
    children: [
      {
        id: "config",
        name: "config",
        kind: "dir",
        checked: true,
        filters: [],
        children: [
          { id: "app.config.js", name: "app.config.js", kind: "file", status: "部分处理", mark: "M", checked: true, filters: ["mod"] },
          { id: "db.config.js", name: "db.config.js", kind: "file", status: "已处理", mark: "M", checked: true, filters: ["mod"] },
          { id: "routes.js", name: "routes.js", kind: "file", status: "未处理", mark: "M", checked: true, filters: ["mod"] },
        ],
      },
      {
        id: "plugins",
        name: "plugins",
        kind: "dir",
        status: "新增目录",
        badge: "4",
        checked: true,
        filters: ["add"],
        children: [
          { id: "api/order.ts", name: "api/order.ts", kind: "file", status: "未处理", mark: "M", checked: true, filters: ["mod"] },
          { id: "utils/format.ts", name: "utils/format.ts", kind: "file", hint: "← lib/format.ts", mark: "R", checked: true, filters: ["move"] },
          { id: "assets/logo.png", name: "assets/logo.png", kind: "file", status: "整文件用右边", mark: "M", checked: true, filters: ["mod"] },
        ],
      },
    ],
  },
  {
    id: "public",
    name: "public",
    kind: "dir",
    badge: "2",
    checked: true,
    filters: [],
    children: [
      { id: "public/index.html", name: "index.html", kind: "file", status: "已处理", mark: "M", checked: true, filters: ["mod"] },
      { id: "favicon.ico", name: "favicon.ico", kind: "file", mark: "A", checked: true, filters: ["add"] },
    ],
  },
  { id: "legacy.sh", name: "legacy.sh", kind: "file", status: "多余", mark: "D", checked: false, filters: ["extra"], extra: true },
  { id: "old-readme.md", name: "old-readme.md", kind: "file", status: "多余", mark: "D", checked: false, filters: ["extra"], extra: true },
  { id: "package.json", name: "package.json", kind: "file", status: "已处理", mark: "M", checked: true, filters: ["mod"] },
];

export const STATS = [
  { n: "6", label: "新增", tone: "add" },
  { n: "14", label: "改动", tone: "mod" },
  { n: "2", label: "多余", tone: "extra" },
  { n: "1", label: "移动", tone: "move" },
] as const;

export const PLAN_SUMMARY = "将新增 6 · 覆盖 13 · 移动 1";
export const BASE_SELECTED = 21;
export const BASE_TOTAL = 23;

export type DiffKind = "ctx" | "del" | "add" | "chg" | "keep" | "edit";
export type HunkId = "import" | "version" | "pool" | "log" | "features";

export type DiffLine = { n: number; text: string; kind: DiffKind; hunk?: HunkId };

const L = (n: number, text: string, kind: DiffKind, hunk?: HunkId): DiffLine => ({ n, text, kind, hunk });

export const ORIGINAL_POOL = "    pool: 10,";
export const EDITED_POOL = "    pool: 16, // 按机器配置调整";

export const LEFT_LINES: DiffLine[] = [
  L(1, "import { defineConfig } from './core';", "ctx"),
  L(2, "", "ctx"),
  L(3, "export default defineConfig({", "ctx"),
  L(4, "  appName: 'OrderCenter',", "ctx"),
  L(5, "  version: '2.3.1',", "del", "version"),
  L(6, "  port: 8080,", "ctx"),
  L(7, "  database: {", "ctx"),
  L(8, "    host: '10.0.3.21',", "ctx"),
  L(9, ORIGINAL_POOL, "del", "pool"),
  L(10, "  },", "ctx"),
  L(11, "  cache: {", "ctx"),
  L(12, "    driver: 'redis',", "ctx"),
  L(13, "    ttl: 3600,", "ctx"),
  L(14, "  },", "ctx"),
  L(15, "  logLevel: 'info',", "keep", "log"),
  L(16, "  // 本地定制：开启审计日志", "keep", "log"),
  L(17, "  audit: true,", "keep", "log"),
  L(18, "  features: ['export', 'report'],", "del", "features"),
  L(19, "});", "ctx"),
];

export const RIGHT_LINES: DiffLine[] = [
  L(1, "import { defineConfig } from './core';", "ctx"),
  L(2, "import { tracing } from './plugins/tracing';", "add", "import"),
  L(3, "", "ctx"),
  L(4, "export default defineConfig({", "ctx"),
  L(5, "  appName: 'OrderCenter',", "ctx"),
  L(6, "  version: '3.0.8',", "chg", "version"),
  L(7, "  port: 8080,", "ctx"),
  L(8, "  database: {", "ctx"),
  L(9, "    host: '10.0.3.21',", "ctx"),
  L(10, "    pool: 20,", "chg", "pool"),
  L(11, "    timeout: 5000,", "add", "pool"),
  L(12, "  },", "ctx"),
  L(13, "  cache: {", "ctx"),
  L(14, "    driver: 'redis',", "ctx"),
  L(15, "    ttl: 3600,", "ctx"),
  L(16, "  },", "ctx"),
  L(17, "  logLevel: 'warn',", "add", "log"),
  L(18, "  plugins: [tracing()],", "add", "log"),
  L(19, "  features: ['export', 'report', 'bi'],", "add", "features"),
  L(20, "});", "ctx"),
];

export type HostStory = {
  id: string;
  name: string;
  user: string;
  host: string;
  port: number;
  os: RemoteOs;
  proxy?: boolean;
  dot: "ok" | "bad" | "off";
  key: string;
  fingerprint: string;
  seen: string;
};

export const HOSTS: HostStory[] = [
  {
    id: "build-01",
    name: "build-01",
    user: "deploy",
    host: "10.0.8.15",
    port: 22,
    os: ALPINE,
    dot: "ok",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:q7Xk2mR9vT0pL+3nYd8wHcF1sZ4aE6uJbN5oK1M2gQw",
    seen: "ED25519 · 9月02日首次确认并记住",
  },
  {
    id: "prod-gw-01",
    name: "prod-gw-01",
    user: "ops",
    host: "172.16.2.40",
    port: 22,
    os: UBUNTU,
    dot: "ok",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:c4kL9sT1pQm8nYb2HcV0eR7uJwA6dF3g",
    seen: "ED25519 · 8月01日首次确认并记住",
  },
  {
    id: "staging-db",
    name: "staging.db",
    user: "root",
    host: "10.0.9.7",
    port: 2222,
    os: UBUNTU,
    dot: "bad",
    key: "~/.ssh/id_ed25519",
    fingerprint: "SHA256:m2Np8Qe1rTs4uYw6HbC0dL5aJfV3kX9z",
    seen: "ED25519 · 7月18日首次确认并记住",
  },
  {
    id: "intranet-jump",
    name: "intranet-jump",
    user: "ops",
    host: "10.1.0.4",
    port: 22,
    os: UBUNTU,
    proxy: true,
    dot: "off",
    key: "",
    fingerprint: "",
    seen: "",
  },
];

export const PREVIEW_STATS = [
  { n: "6", label: "新增" },
  { n: "13", label: "覆盖", sub: "其中合并 4" },
  { n: "1", label: "移动" },
  { n: "0", label: "删除" },
];

export const SCAN_TITLE = "扫描后原项目有 1 个文件被改动";
export const SCAN_LINE = "src/routes.js · 修改时间 14:32 → 14:47";
export const PREVIEW_NOTE = "app.config.js 部分处理，其余 2 处差异将按默认（用右边）处理。";

export type ChangeRow = { kind: "新增" | "覆盖" | "移动"; path: string; source: string; keepOld?: boolean };

export const PREVIEW_ROWS: ChangeRow[] = [
  { kind: "新增", path: "plugins/tracing/index.ts", source: "升级包" },
  { kind: "新增", path: "plugins/tracing/exporter.ts", source: "升级包" },
  { kind: "覆盖", path: "src/config/app.config.js", source: "合并结果" },
  { kind: "覆盖", path: "src/config/db.config.js", source: "合并结果" },
  { kind: "覆盖", path: "src/routes.js", source: "升级包" },
  { kind: "覆盖", path: "public/index.html", source: "升级包" },
  { kind: "移动", path: "lib/format.ts → utils/format.ts", source: "合并结果", keepOld: true },
  { kind: "覆盖", path: "package.json", source: "合并结果" },
];

export const STOPPED = {
  done: 9,
  total: 20,
  failed: 1,
  file: "src/config/app.yaml",
  code: "E_VERIFY_FAILED",
  message: "写入后的哈希和升级包不一致",
  manifest: "备份清单 manifest.json 完整",
  side: "原来的事务和备份都还在",
  when: "今天 13:52",
  where: "~/work/order-center ← release-3.1.0.zip",
};

export const ROLLBACK_PROGRESS = {
  done: 6,
  total: 9,
  side: "备份都还在",
  when: "今天 14:31",
  where: "~/work/order-center ← release-3.1.0.zip",
};

export const CONFLICT_MODIFIED: ResumeFile[] = [
  { path: "src/config/db.config.js", detail: "升级后修改 · 今天 13:58" },
  { path: "src/routes.js", detail: "升级后修改 · 今天 13:57" },
];

export const CONFLICT_DELETED: ResumeFile[] = [
  { path: "src/config/db.config.js", detail: "升级后修改 · 今天 13:58" },
  { path: "src/routes.js", detail: "升级后被删除 · 今天 13:57", deleted: true },
];

export const ROLLBACK_FILES: ResumeFile[] = [
  { path: "src/routes.js", detail: "升级后修改于 10月14日 09:12" },
  { path: "public/index.html", detail: "升级后修改于 10月13日 17:40" },
  { path: "conf/legacy.yml", detail: "升级后被删除 · 10月15日 11:03", deleted: true },
];

export const ROLLBACK_RECREATED: ResumeFile[] = [
  ...ROLLBACK_FILES,
  { path: "src/config/app.config.js", detail: RECREATED_REASON },
];

export const PENDING_FILES: ResumeFile[] = [
  { path: "src/config/app.yaml", detail: "停下后修改 · 今天 14:05" },
  { path: "public/index.html", detail: "停下后修改 · 今天 14:02" },
];

export const RESUME_FILE = "src/config/app.yaml";

export type HistoryAction = "rollback" | "continue" | "cleaned" | "unrestored";

export type HistoryRow = {
  id: string;
  time: string;
  where: string;
  change: string;
  result: string;
  tone: "fail" | "ok" | "rolled" | "partial";
  actions: HistoryAction[];
  unrestored?: UnrestoredFile[];
  attr?: UnrestoredFile[];
  /** 完全回滚时，权限备注直接出现在详情里。 */
  attrDirect?: boolean;
};

const SUCCESS_TAIL: HistoryRow[] = [
  { id: "1012", time: "10月12日 14:20", where: "~/work/order-center ← release-3.0.0.zip", change: "新增 6 · 覆盖 13 · 移动 1", result: "成功", tone: "ok", actions: ["rollback"] },
  { id: "0928", time: "9月28日 09:41", where: "~/work/order-center ← release-2.3.1.zip", change: "新增 2 · 覆盖 8", result: "成功", tone: "ok", actions: ["rollback"] },
  { id: "0903", time: "9月03日 18:05", where: "prod-gw-01:/opt/gateway ← gateway-1.8", change: "覆盖 21 · 删除 3", result: "已回滚", tone: "rolled", actions: ["rollback", "cleaned"] },
  { id: "0811", time: "8月11日 10:12", where: "~/work/order-center ← release-2.2.0.zip", change: "新增 4 · 覆盖 17", result: "成功", tone: "ok", actions: ["rollback", "cleaned"] },
];

export const HISTORY_FAILURE: HistoryRow[] = [
  { id: "fail", time: "今天 13:52", where: "~/work/order-center ← release-3.1.0.zip", change: "已写 9 / 20 · 失败 1", result: "部分失败", tone: "fail", actions: ["rollback", "continue"] },
  ...SUCCESS_TAIL,
];

export const HISTORY_PARTIAL: HistoryRow[] = [
  {
    id: "partial",
    time: "今天 16:40",
    where: "~/work/order-center ← release-3.1.0.zip",
    change: PARTIAL_ROLLBACK_CHANGE,
    result: PARTIAL_ROLLBACK_RESULT,
    tone: "partial",
    actions: ["unrestored"],
    unrestored: PARTIAL_ROLLBACK_UNRESTORED,
  },
  ...SUCCESS_TAIL,
];

export const HISTORY_ATTR: HistoryRow[] = [
  { ...HISTORY_PARTIAL[0], attr: ATTR_UNRESTORED },
  ...SUCCESS_TAIL,
];

export const HISTORY_ROLLED: HistoryRow[] = [
  {
    id: "rolled",
    time: "今天 16:40",
    where: "~/work/order-center ← release-3.1.0.zip",
    change: "已恢复 9 / 9",
    result: "已回滚",
    tone: "rolled",
    actions: [],
    attr: ATTR_UNRESTORED,
    attrDirect: true,
  },
  ...SUCCESS_TAIL,
];
