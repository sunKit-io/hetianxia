import { useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, ChevronUp, Folder, FolderOpen, History, Search } from "lucide-react";
import {
  BASE_SELECTED,
  BASE_TOTAL,
  EDITED_POOL,
  LEFT_LINES,
  ORIGINAL_POOL,
  PLAN_SUMMARY,
  PROJECTS,
  RIGHT_LINES,
  SCHEME_NAME,
  SIDE_HISTORY,
  STATS,
  TREE,
  type DiffLine,
  type Facet,
  type HunkId,
  type TreeNode,
} from "./demoData";
import { mapDocY } from "./syncScroll";

type Choice = "right" | "left" | "both" | "partial" | "open";
type Order = "left-top" | "right-top";
type Filter = "all" | Facet;

const INITIAL: Record<HunkId, Choice> = {
  import: "right",
  version: "right",
  pool: "partial",
  log: "open",
  features: "open",
};

const HUNK_ORDER: HunkId[] = ["import", "version", "pool", "log", "features"];

export function EditorTools({
  query,
  onQuery,
  onHistory,
}: {
  query: string;
  onQuery: (value: string) => void;
  onHistory: () => void;
}) {
  return (
    <>
      <Search size={16} />
      <button type="button" className="iconbtn" aria-label="历史" onClick={onHistory}>
        <History size={16} />
      </button>
      <input className="titlesearch" placeholder="搜索文件" aria-label="搜索文件" value={query} onChange={(e) => onQuery(e.target.value)} />
    </>
  );
}

export function EditorScreen({
  initialEdited,
  query,
  onUpgrade,
  onOpenHistory,
  onOpenRollback,
  onOpenSsh,
}: {
  initialEdited: boolean;
  query: string;
  onUpgrade: () => void;
  onOpenHistory: () => void;
  onOpenRollback: () => void;
  onOpenSsh: () => void;
}) {
  const [filter, setFilter] = useState<Filter>("all");
  const [large, setLarge] = useState(false);
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [checks, setChecks] = useState<Record<string, boolean>>(() => initChecks(TREE));
  const [allowDelete, setAllowDelete] = useState(false);
  const [fileId, setFileId] = useState("app.config.js");
  const [choice, setChoice] = useState<Record<HunkId, Choice>>(INITIAL);
  const [active, setActive] = useState<HunkId>("features");
  const [order, setOrder] = useState<Order>("left-top");
  const [picked, setPicked] = useState(1);
  const [tab, setTab] = useState<"file" | "merge">("file");
  const [pool, setPool] = useState(initialEdited ? EDITED_POOL : ORIGINAL_POOL);
  const edited = pool !== ORIGINAL_POOL;
  const files = useMemo(() => flatten(TREE).filter((node) => node.kind === "file"), []);
  const initialChecked = files.filter((node) => node.checked).length;
  const checkedNow = files.filter((node) => checks[node.id]).length;
  const selected = BASE_SELECTED + (checkedNow - initialChecked);
  const done = HUNK_ORDER.filter((id) => choice[id] !== "open").length;

  const setHunk = (next: Choice) => setChoice((prev) => ({ ...prev, [active]: next }));
  const resetAll = () => {
    setChoice(INITIAL);
    setOrder("left-top");
    setPool(ORIGINAL_POOL);
    setActive("features");
  };
  const bulk = (next: Choice) => {
    const all = {} as Record<HunkId, Choice>;
    for (const id of HUNK_ORDER) all[id] = next;
    setChoice(all);
  };

  return (
    <div className="workspace">
      <aside className="nav">
        <p className="sidehead">项目</p>
        {PROJECTS.map((project) => (
          <button
            key={project.id}
            type="button"
            className={`siderow ${project.id === "order" ? "sel" : ""}`}
            onClick={() => {
              if (project.id === "gateway") onOpenSsh();
            }}
          >
            <Folder size={15} />
            <span>{project.name}</span>
            {project.badge && <b className="count">{project.badge}</b>}
          </button>
        ))}
        <p className="sidehead">合并方案</p>
        <button type="button" className="siderow quietrow">{SCHEME_NAME}</button>
        <p className="sidehead">历史记录</p>
        {SIDE_HISTORY.map((item) => (
          <div key={item.id} className="siderow hist">
            <button type="button" onClick={onOpenHistory}>
              <i className={item.ok ? "tick" : "dotline"} />
              <span>{item.label}</span>
            </button>
            {item.rollback && <button type="button" className="textbtn" onClick={onOpenRollback}>回滚</button>}
          </div>
        ))}
      </aside>
      <section className="files">
        <div className="stats">
          {STATS.map((item) => (
            <b key={item.label} className={item.tone}><span>{item.n}</span><small>{item.label}</small></b>
          ))}
        </div>
        <div className="filters">
          {(["all", "add", "mod", "extra", "move"] as const).map((item) => (
            <button key={item} type="button" className={filter === item ? "on" : ""} onClick={() => setFilter(item)}>
              {item === "all" ? "全部" : item === "add" ? "新增" : item === "mod" ? "改动" : item === "extra" ? "多余" : "移动"}
            </button>
          ))}
          <button type="button" className={`lg ${large ? "on" : ""}`} onClick={() => setLarge((v) => !v)} aria-pressed={large}>lg</button>
        </div>
        <div className="tree">
          {filterTree(TREE, filter, query).map((node) => (
            <TreeRow
              key={node.id}
              node={node}
              depth={0}
              open={open}
              setOpen={setOpen}
              checks={checks}
              allowDelete={allowDelete}
              fileId={fileId}
              edited={edited}
              onToggle={(id, extra) => {
                if (extra && !allowDelete && !checks[id]) return;
                setChecks((prev) => ({ ...prev, [id]: !prev[id] }));
              }}
              onOpen={setFileId}
            />
          ))}
        </div>
      </section>
      <section className={`diffcol ${large ? "lgtext" : ""}`}>
        <header className="diffhead">
          <div className="filetitle">
            <span>{fileId}</span>
            <i className={edited ? "pill edit" : "pill mod"}>{edited ? "已编辑" : "改动"}</i>
          </div>
          <span className="progress">已处理 {done} / 共 {HUNK_ORDER.length} 处</span>
          <div className="bulk">
            <button type="button" onClick={() => bulk("right")}>全部用右边</button>
            <button type="button" onClick={() => bulk("left")}>全部保留左边</button>
            <button type="button" onClick={resetAll}>全部恢复</button>
          </div>
        </header>
        <div className="diffsub">
          <div className="tabs">
            <button type="button" className={tab === "file" ? "on" : ""} onClick={() => setTab("file")}>原文件</button>
            <button type="button" className={tab === "merge" ? "on" : ""} onClick={() => setTab("merge")}>合并结果预览</button>
            <span>原始项目 · 本地</span>
          </div>
          <span className="pack">升级包 ← release-3.0.0.zip</span>
          <span className="enc">UTF-8 · LF · JavaScript</span>
        </div>
        {fileId === "app.config.js" ? (
          tab === "file" ? (
            <DiffPane
              pool={pool}
              onPool={setPool}
              choice={choice}
              active={active}
              order={order}
              picked={picked}
              onActive={setActive}
              onChoice={setHunk}
              onOrder={setOrder}
              onPicked={setPicked}
            />
          ) : (
            <pre className="merge">{mergeText(choice, pool, order)}</pre>
          )
        ) : (
          <div className="emptyfile">
            <p>{fileId}</p>
            <p>这一处还没有打开左右对照。</p>
          </div>
        )}
        <footer className="dock">
          <label className="switch">
            <input type="checkbox" checked={allowDelete} onChange={(e) => setAllowDelete(e.target.checked)} />
            <i />
            允许删除多余文件
          </label>
          <span className="picked">已选 {selected} / {BASE_TOTAL}</span>
          <span className="legend">
            <i className="lg-right" />用右边
            <i className="lg-left" />保留左边
            <i className="lg-part" />部分处理
            <i className="lg-cur" />当前块
            {edited && <><i className="lg-edit" />已编辑</>}
          </span>
          <span className="summary">{PLAN_SUMMARY}</span>
          <button type="button" className="primary" onClick={onUpgrade}>执行升级</button>
        </footer>
      </section>
    </div>
  );
}

function TreeRow({
  node,
  depth,
  open,
  setOpen,
  checks,
  allowDelete,
  fileId,
  edited,
  onToggle,
  onOpen,
}: {
  node: TreeNode;
  depth: number;
  open: Record<string, boolean>;
  setOpen: (fn: (prev: Record<string, boolean>) => Record<string, boolean>) => void;
  checks: Record<string, boolean>;
  allowDelete: boolean;
  fileId: string;
  edited: boolean;
  onToggle: (id: string, extra: boolean) => void;
  onOpen: (id: string) => void;
}) {
  const expanded = open[node.id] !== false;
  const status = node.id === "app.config.js" && edited ? "已编辑" : node.status;
  return (
    <div>
      <div className={`filerow ${node.id === fileId ? "sel" : ""}`} style={{ paddingLeft: 8 + depth * 14 }}>
        {node.kind === "dir" ? (
          <button type="button" className="twist" aria-label={expanded ? "折叠" : "展开"} onClick={() => setOpen((prev) => ({ ...prev, [node.id]: !expanded }))}>
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        ) : <span className="twistgap" />}
        {node.kind === "file" ? (
          <input
            type="checkbox"
            checked={!!checks[node.id]}
            disabled={node.extra && !allowDelete && !checks[node.id]}
            onChange={() => onToggle(node.id, !!node.extra)}
            aria-label={node.name}
          />
        ) : <span className="twistgap" />}
        {node.kind === "dir" ? (expanded ? <FolderOpen size={14} /> : <Folder size={14} />) : null}
        <button type="button" className="filename" onClick={() => node.kind === "file" && onOpen(node.id)}>{node.name}</button>
        {node.hint && <em className="hint">{node.hint}</em>}
        <span className="rowmeta">
          {status && <em className={`status ${status === "已编辑" ? "edit" : status === "部分处理" ? "part" : ""}`}>{status}</em>}
          {node.badge && node.kind === "dir" && <em className="badge">{node.badge}</em>}
          {node.mark && <i className={`mark ${node.mark}`}>{node.mark}</i>}
        </span>
      </div>
      {node.kind === "dir" && expanded && node.children?.map((child) => (
        <TreeRow
          key={child.id}
          node={child}
          depth={depth + 1}
          open={open}
          setOpen={setOpen}
          checks={checks}
          allowDelete={allowDelete}
          fileId={fileId}
          edited={edited}
          onToggle={onToggle}
          onOpen={onOpen}
        />
      ))}
    </div>
  );
}

function DiffPane({
  pool,
  onPool,
  choice,
  active,
  order,
  picked,
  onActive,
  onChoice,
  onOrder,
  onPicked,
}: {
  pool: string;
  onPool: (value: string) => void;
  choice: Record<HunkId, Choice>;
  active: HunkId;
  order: Order;
  picked: number;
  onActive: (id: HunkId) => void;
  onChoice: (choice: Choice) => void;
  onOrder: (order: Order) => void;
  onPicked: (n: number) => void;
}) {
  const leftRef = useRef<HTMLDivElement>(null);
  const rightRef = useRef<HTMLDivElement>(null);
  const lock = useRef<"left" | "right" | null>(null);
  const anchors = useMemo(() => [
    { left: 0, right: 0 },
    { left: LEFT_LINES.length * 22, right: RIGHT_LINES.length * 22 },
  ], []);
  const sync = (side: "left" | "right") => {
    if (lock.current && lock.current !== side) return;
    const src = side === "left" ? leftRef.current : rightRef.current;
    const dst = side === "left" ? rightRef.current : leftRef.current;
    if (!src || !dst) return;
    lock.current = side;
    const mapped = side === "left"
      ? mapDocY(anchors, src.scrollTop)
      : mapDocY(anchors.map((item) => ({ left: item.right, right: item.left })), src.scrollTop);
    if (Math.abs(dst.scrollTop - mapped) > 0.5) dst.scrollTop = mapped;
    requestAnimationFrame(() => {
      if (lock.current === side) lock.current = null;
    });
  };
  const left = LEFT_LINES.map((line) => line.hunk === "pool" ? { ...line, text: pool, kind: pool === ORIGINAL_POOL ? line.kind : "edit" as const } : line);
  return (
    <div className="panes">
      <div className="pane" ref={leftRef} onScroll={() => sync("left")}>
        {left.map((line) => <CodeLine key={line.n} line={line} current={line.hunk === active} editable={line.hunk === "pool"} onEdit={onPool} onPick={() => line.hunk && onActive(line.hunk)} />)}
      </div>
      <div className="gutter" role="toolbar" aria-label="冲突处理">
        <button type="button" className={choice[active] === "right" ? "on" : ""} onClick={() => onChoice("right")}>用右边</button>
        <button type="button" className={choice[active] === "left" || choice[active] === "open" ? "dark" : ""} onClick={() => onChoice("left")}>保留左边</button>
        <button type="button" className={choice[active] === "both" ? "on" : ""} onClick={() => onChoice("both")}>都保留</button>
        <button type="button" onClick={() => onChoice(INITIAL[active])}>恢复</button>
        {choice[active] === "partial" && <em className="partchip">部分</em>}
        {choice[active] === "both" && (
          <div className="bothorder">
            <button type="button" className={order === "left-top" ? "on" : ""} onClick={() => onOrder("left-top")}>左边在上</button>
            <button type="button" className={order === "right-top" ? "on" : ""} onClick={() => onOrder("right-top")}>右边在上</button>
          </div>
        )}
        <div className="pickline">
          <button type="button" aria-label="上一行" onClick={() => onPicked(Math.max(1, picked - 1))}><ChevronUp size={14} /></button>
          <span>选中 {picked} 行</span>
          <button type="button" aria-label="下一行" onClick={() => onPicked(picked + 1)}><ChevronDown size={14} /></button>
        </div>
      </div>
      <div className="pane" ref={rightRef} onScroll={() => sync("right")}>
        {RIGHT_LINES.map((line) => <CodeLine key={line.n} line={line} current={line.hunk === active} onPick={() => line.hunk && onActive(line.hunk)} />)}
      </div>
    </div>
  );
}

function CodeLine({ line, current, editable, onEdit, onPick }: { line: DiffLine; current: boolean; editable?: boolean; onEdit?: (value: string) => void; onPick?: () => void }) {
  return (
    <div className={`dline ${line.kind} ${current ? "current" : ""}`} onClick={onPick}>
      <span className="ln">{line.n}</span>
      {editable && onEdit ? (
        <input className="lineedit" value={line.text} onChange={(e) => onEdit(e.target.value)} aria-label="连接池" spellCheck={false} />
      ) : (
        <span className="lt">{line.text}</span>
      )}
      {line.kind === "edit" && <em className="editpill">已编辑</em>}
    </div>
  );
}

function mergeText(choice: Record<HunkId, Choice>, pool: string, order: Order): string {
  const lines = ["import { defineConfig } from './core';"];
  if (choice.import !== "left") lines.push("import { tracing } from './plugins/tracing';");
  lines.push("", "export default defineConfig({", "  appName: 'OrderCenter',");
  const versionLeft = "  version: '2.3.1',";
  const versionRight = "  version: '3.0.8',";
  lines.push(...pick(choice.version, versionLeft, versionRight, order));
  lines.push("  port: 8080,", "  database: {", "    host: '10.0.3.21',");
  const poolLeft = pool;
  const poolRight = "    pool: 20,\n    timeout: 5000,";
  lines.push(...pick(choice.pool === "open" ? "partial" : choice.pool, poolLeft, poolRight, order));
  lines.push("  },", "  cache: {", "    driver: 'redis',", "    ttl: 3600,", "  },");
  const logLeft = "  logLevel: 'info',\n  // 本地定制：开启审计日志\n  audit: true,";
  const logRight = "  logLevel: 'warn',\n  plugins: [tracing()],";
  lines.push(...pick(choice.log, logLeft, logRight, order));
  const featLeft = "  features: ['export', 'report'],";
  const featRight = "  features: ['export', 'report', 'bi'],";
  lines.push(...pick(choice.features, featLeft, featRight, order));
  lines.push("});");
  return lines.join("\n");
}

function pick(choice: Choice, left: string, right: string, order: Order): string[] {
  if (choice === "left") return left.split("\n");
  if (choice === "both") {
    const pair = order === "left-top" ? [left, right] : [right, left];
    return pair.flatMap((block) => block.split("\n"));
  }
  if (choice === "partial") return left.split("\n");
  return right.split("\n");
}

function flatten(nodes: TreeNode[]): TreeNode[] {
  return nodes.flatMap((node) => [node, ...(node.children ? flatten(node.children) : [])]);
}

function initChecks(nodes: TreeNode[]): Record<string, boolean> {
  const out: Record<string, boolean> = {};
  for (const node of flatten(nodes)) out[node.id] = node.checked;
  return out;
}

function filterTree(nodes: TreeNode[], filter: Filter, query: string): TreeNode[] {
  const q = query.trim().toLowerCase();
  const keep = (node: TreeNode): TreeNode | null => {
    const children = node.children?.map(keep).filter((item): item is TreeNode => item !== null);
    const facet = filter === "all" || node.filters.includes(filter) || (children?.length ?? 0) > 0;
    const named = !q || node.name.toLowerCase().includes(q) || (children?.length ?? 0) > 0;
    if (!facet || !named) return null;
    return children ? { ...node, children } : node;
  };
  return nodes.map(keep).filter((item): item is TreeNode => item !== null);
}
