import { EditorState, Compartment } from "@codemirror/state";
import {
  EditorView,
  lineNumbers,
  drawSelection,
  keymap,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  codeFolding,
  foldEffect,
  foldGutter,
  syntaxHighlighting,
  defaultHighlightStyle,
  bracketMatching,
} from "@codemirror/language";
import { javascript } from "@codemirror/lang-javascript";

export type Hunk = {
  id: number;
  /** 1-based inclusive. leftEnd === leftStart means a pure insertion. */
  leftStart: number;
  leftEnd: number;
  rightStart: number;
  rightEnd: number;
};

type Anchor = { left: number; right: number };

type Sample = {
  name: string;
  maxRibbonPx: number;
  maxSyncPx: number;
  samples: number;
};

type SpikeResults = {
  generatedAt: string;
  userAgent: string;
  doc: {
    leftLines: number;
    rightLines: number;
    leftBytes: number;
    rightBytes: number;
    hunks: number;
    pureInserts: number;
  };
  open: {
    buildMs: number;
    stateMs: number;
    viewMs: number;
    firstContentMs: number;
    highlightClassMs: number | null;
    withSyntax: boolean;
  };
  scrollHandler: { p95Ms: number; maxMs: number; n: number };
  foldMs: number;
  scenarios: Sample[];
  maxErrorPx: number;
  pass: boolean;
  notes: string[];
};

declare global {
  interface Window {
    __SPIKE_RESULTS__?: SpikeResults;
    __SPIKE_DONE__?: boolean;
  }
}

const TARGET_LINES = 100_000;
const statusEl = document.querySelector<HTMLElement>("#status")!;
const leftHost = document.querySelector<HTMLElement>("#left")!;
const rightHost = document.querySelector<HTMLElement>("#right")!;
const gutter = document.querySelector<HTMLElement>("#gutter")!;
const svg = document.querySelector<SVGSVGElement>("#ribbon")!;
const probe = document.querySelector<HTMLElement>("#probe")!;

function pad(n: number, width: number): string {
  const s = String(n);
  return s.length >= width ? s : " ".repeat(width - s.length) + s;
}

/** Build ~5MB / 100k-line documents with asymmetric hunks so line numbers diverge. */
function buildDocs(): { left: string; right: string; hunks: Hunk[] } {
  const left: string[] = [];
  const right: string[] = [];
  const hunks: Hunk[] = [];
  let li = 1;
  let ri = 1;
  let guard = 0;
  while (li <= TARGET_LINES && guard < TARGET_LINES * 2) {
    guard++;
    const unchanged = 380;
    for (let i = 0; i < unchanged && li <= TARGET_LINES; i++) {
      const text = `function keep_${li}() { return ${li}; } // stable line payload`;
      left.push(text);
      right.push(text);
      li++;
      ri++;
    }
    if (li > TARGET_LINES) break;
    const id = hunks.length;
    const pure = id % 11 === 10;
    if (pure) {
      const rStart = ri;
      for (let i = 0; i < 3; i++) {
        right.push(`function inserted_${ri}_${i}() { return "new"; } // ++++++`);
        ri++;
      }
      hunks.push({ id, leftStart: li, leftEnd: li, rightStart: rStart, rightEnd: ri });
    } else {
      const lStart = li;
      const rStart = ri;
      const ln = 2 + (id % 4);
      const rn = 1 + (id % 3);
      for (let i = 0; i < ln && li <= TARGET_LINES + 8; i++) {
        left.push(`function left_${li}_${i}() { return "old-${i}"; } // ------`);
        li++;
      }
      for (let i = 0; i < rn; i++) {
        right.push(`function right_${ri}_${i}() { return "new-${i}"; } // ++++++`);
        ri++;
      }
      hunks.push({ id, leftStart: lStart, leftEnd: li, rightStart: rStart, rightEnd: ri });
    }
  }
  return { left: left.join("\n") + "\n", right: right.join("\n") + "\n", hunks };
}

function zoomFactor(): number {
  const z = probe.getBoundingClientRect().height / probe.offsetHeight;
  return z > 0.01 ? z : 1;
}

function theme(fontSize: string) {
  return EditorView.theme({
    "&": { height: "100%", fontSize, background: "#fff" },
    ".cm-scroller": {
      overflow: "auto",
      fontFamily: 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, monospace',
      lineHeight: "1.45",
    },
    ".cm-gutters": { background: "#fafafa", borderRight: "1px solid #ececf0" },
    ".cm-line": { padding: "0 8px" },
  });
}

const fontCompartment = new Compartment();
const syntaxCompartment = new Compartment();

function baseExtensions(withSyntax: boolean) {
  return [
    lineNumbers(),
    drawSelection(),
    history(),
    keymap.of([...defaultKeymap, ...historyKeymap]),
    codeFolding(),
    foldGutter(),
    bracketMatching(),
    EditorView.editable.of(false),
    fontCompartment.of(theme("13px")),
    syntaxCompartment.of(
      withSyntax
        ? [javascript(), syntaxHighlighting(defaultHighlightStyle)]
        : [],
    ),
  ];
}

/** 行块坐标换算成 scrollTop 使用的 CSS 像素。CSS zoom 下 CM6 的 lineBlock 会乘上 scaleY。 */
function scrollPx(view: EditorView, docPx: number): number {
  const scale = view.scaleY || 1;
  return docPx / scale;
}

function docTopOfLine(view: EditorView, lineNo: number): number {
  const max = view.state.doc.lines;
  if (lineNo < 1) return 0;
  if (lineNo > max) {
    const last = view.state.doc.line(max);
    return scrollPx(view, view.lineBlockAt(last.from).bottom);
  }
  const line = view.state.doc.line(lineNo);
  return scrollPx(view, view.lineBlockAt(line.from).top);
}

function docBottomOfRange(view: EditorView, start: number, end: number): number {
  if (end <= start) return docTopOfLine(view, start);
  const line = view.state.doc.line(end - 1);
  return scrollPx(view, view.lineBlockAt(line.from).bottom);
}

function buildAnchors(left: EditorView, right: EditorView, hunks: Hunk[]): Anchor[] {
  const anchors: Anchor[] = [{ left: 0, right: 0 }];
  for (const h of hunks) {
    anchors.push({
      left: docTopOfLine(left, h.leftStart),
      right: docTopOfLine(right, h.rightStart),
    });
    anchors.push({
      left: docBottomOfRange(left, h.leftStart, h.leftEnd),
      right: docBottomOfRange(right, h.rightStart, h.rightEnd),
    });
  }
  const leftLast = left.state.doc.line(left.state.doc.lines);
  const rightLast = right.state.doc.line(right.state.doc.lines);
  anchors.push({
    left: left.lineBlockAt(leftLast.from).bottom,
    right: right.lineBlockAt(rightLast.from).bottom,
  });
  anchors.sort((a, b) => a.left - b.left || a.right - b.right);
  const dedup: Anchor[] = [];
  for (const a of anchors) {
    const prev = dedup[dedup.length - 1];
    if (prev && Math.abs(prev.left - a.left) < 0.01 && Math.abs(prev.right - a.right) < 0.01) {
      continue;
    }
    dedup.push(a);
  }
  return dedup;
}

function mapDocY(anchors: Anchor[], y: number): number {
  if (anchors.length === 0) return y;
  if (y <= anchors[0].left) return anchors[0].right;
  const last = anchors[anchors.length - 1];
  if (y >= last.left) return last.right + (y - last.left);
  let lo = 0;
  let hi = anchors.length - 1;
  while (lo + 1 < hi) {
    const mid = (lo + hi) >> 1;
    if (anchors[mid].left <= y) lo = mid;
    else hi = mid;
  }
  const a = anchors[lo];
  const b = anchors[hi];
  const span = b.left - a.left;
  const t = span === 0 ? 0 : (y - a.left) / span;
  return a.right + t * (b.right - a.right);
}

let syncing: "left" | "right" | "lock" | null = null;
let anchors: Anchor[] = [];
let leftView: EditorView;
let rightView: EditorView;
let hunks: Hunk[] = [];
const scrollCosts: number[] = [];

function syncScroll(from: "left" | "right") {
  const t0 = performance.now();
  // lock 期间忽略回声，否则修正帧会被锚点映射立刻盖掉。
  if (syncing) return;
  syncing = from;
  const src = from === "left" ? leftView : rightView;
  const dst = from === "left" ? rightView : leftView;
  const srcY = src.scrollDOM.scrollTop;
  const mapped = from === "left"
    ? mapDocY(anchors, srcY)
    : mapDocY(
        anchors.map((a) => ({ left: a.right, right: a.left })),
        srcY,
      );
  if (Math.abs(dst.scrollDOM.scrollTop - mapped) > 0.25) {
    dst.scrollDOM.scrollTop = mapped;
  }
  syncing = null;
  scrollCosts.push(performance.now() - t0);
}

function screenTopForLine(view: EditorView, lineNo: number): number | null {
  const max = view.state.doc.lines;
  const n = Math.min(Math.max(lineNo, 1), max);
  const line = view.state.doc.line(n);
  const coords = view.coordsAtPos(line.from);
  return coords ? coords.top : null;
}

function placeRibbons(): { maxError: number; count: number } {
  const host = gutter.getBoundingClientRect();
  const z = zoomFactor();
  const w = gutter.clientWidth;
  const h = gutter.clientHeight;
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.setAttribute("width", String(w));
  svg.setAttribute("height", String(h));
  const parts: string[] = [];
  let maxError = 0;
  let count = 0;
  gutter.querySelectorAll(".tick").forEach((n) => n.remove());

  for (const hunk of hunks) {
    const l1 = screenTopForLine(leftView, hunk.leftStart);
    const r1 = screenTopForLine(rightView, hunk.rightStart);
    if (l1 == null || r1 == null) continue;
    const lLine = leftView.state.doc.line(Math.min(hunk.leftStart, leftView.state.doc.lines));
    const rLine = rightView.state.doc.line(Math.min(hunk.rightStart, rightView.state.doc.lines));
    const lBox = leftView.lineBlockAt(lLine.from);
    const rBox = rightView.lineBlockAt(rLine.from);
    let l2: number;
    let r2: number;
    if (hunk.leftEnd <= hunk.leftStart) {
      l2 = l1;
    } else {
      const end = leftView.state.doc.line(hunk.leftEnd - 1);
      const c = leftView.coordsAtPos(end.from);
      l2 = c ? c.bottom : l1 + lBox.height;
    }
    if (hunk.rightEnd <= hunk.rightStart) {
      r2 = r1;
    } else {
      const end = rightView.state.doc.line(hunk.rightEnd - 1);
      const c = rightView.coordsAtPos(end.from);
      r2 = c ? c.bottom : r1 + rBox.height;
    }
    if (l2 < host.top - 2 && l1 < host.top - 2) continue;
    if (l1 > host.bottom + 2 && l2 > host.bottom + 2) continue;

    const y1 = (l1 - host.top) / z;
    const y2 = (r1 - host.top) / z;
    const y1b = (l2 - host.top) / z;
    const y2b = (r2 - host.top) / z;
    const x0 = 0;
    const x1 = w;
    const c1 = w * 0.45;
    const c2 = w * 0.55;
    parts.push(
      `<path data-hunk="${hunk.id}" d="M ${x0} ${y1} C ${c1} ${y1}, ${c2} ${y2}, ${x1} ${y2} L ${x1} ${y2b} C ${c2} ${y2b}, ${c1} ${y1b}, ${x0} ${y1b} Z" fill="rgba(0,122,255,0.18)" stroke="rgba(0,122,255,0.55)" stroke-width="1" />`,
    );

    const tick = document.createElement("div");
    tick.className = "tick";
    tick.dataset.hunk = String(hunk.id);
    tick.style.top = `${y1}px`;
    gutter.appendChild(tick);
    const visual = Math.abs(tick.getBoundingClientRect().top - l1);
    const tickR = document.createElement("div");
    tickR.className = "tick";
    tickR.style.top = `${y2}px`;
    gutter.appendChild(tickR);
    const visualR = Math.abs(tickR.getBoundingClientRect().top - r1);
    maxError = Math.max(maxError, visual, visualR);
    count++;
  }
  svg.innerHTML = parts.join("");
  return { maxError, count };
}

function percentile(values: number[], p: number): number {
  if (values.length === 0) return 0;
  const s = [...values].sort((a, b) => a - b);
  const i = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
  return s[i];
}

async function waitLayout(view: EditorView): Promise<void> {
  await new Promise<void>((resolve) => {
    view.requestMeasure({
      read: () => null,
      write: () => resolve(),
    });
  });
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
}

/** 字号、折叠、缩放之后高度表要连续两帧不变才算稳定。 */
async function settle(view: EditorView): Promise<void> {
  let prev = -1;
  for (let i = 0; i < 8; i++) {
    await waitLayout(view);
    const h = Math.round(view.contentHeight);
    if (h === prev) return;
    prev = h;
  }
}

function pixelScale(view: EditorView): number {
  const s = view.scaleY || 1;
  const z = zoomFactor();
  return s > 1.01 ? s : z;
}

function scrollLineTo(view: EditorView, lineNo: number, offsetPx: number) {
  const max = view.state.doc.lines;
  const n = Math.min(Math.max(1, lineNo), max);
  const line = view.state.doc.line(n);
  const target = Math.max(0, docTopOfLine(view, n) - offsetPx);
  if (Math.abs(view.scrollDOM.scrollTop - target) > 0.5) {
    view.scrollDOM.scrollTop = target;
  }
  void line;
}

/**
 * 分段线性映射先跳到锚点，再用一帧屏幕坐标修正。
 * 字号变化后 CM6 会按滚动锚点改 scrollTop，只信映射会留出几十像素；
 * 修正量 = (右栏屏幕 Y - 左栏屏幕 Y) / scale。
 */
async function syncClosed(leftLine: number, rightLine: number): Promise<number> {
  syncing = "lock";
  try {
    anchors = buildAnchors(leftView, rightView, hunks);
    const srcY = leftView.scrollDOM.scrollTop;
    const mapped = mapDocY(anchors, srcY);
    if (Math.abs(rightView.scrollDOM.scrollTop - mapped) > 0.25) {
      rightView.scrollDOM.scrollTop = mapped;
    }
    await waitLayout(rightView);
    const beforeL = screenTopForLine(leftView, leftLine);
    const beforeR = screenTopForLine(rightView, rightLine);
    const open = beforeL != null && beforeR != null ? Math.abs(beforeL - beforeR) : -1;
    if (beforeL != null && beforeR != null && Math.abs(beforeR - beforeL) > 0.5) {
      rightView.scrollDOM.scrollTop += (beforeR - beforeL) / pixelScale(rightView);
      await waitLayout(rightView);
      const afterL = screenTopForLine(leftView, leftLine);
      const afterR = screenTopForLine(rightView, rightLine);
      if (afterL != null && afterR != null && Math.abs(afterR - afterL) > 1) {
        // 高度表在修正期间又变了，再补一帧。
        rightView.scrollDOM.scrollTop += (afterR - afterL) / pixelScale(rightView);
        await waitLayout(rightView);
      }
    }
    return open;
  } finally {
    syncing = null;
  }
}

async function measureScenario(name: string, prepare: () => Promise<void> | void): Promise<Sample> {
  await prepare();
  await settle(leftView);
  await settle(rightView);
  anchors = buildAnchors(leftView, rightView, hunks);
  const errorsRibbon: number[] = [];
  const errorsSync: number[] = [];
  const openLoop: number[] = [];
  const picks = hunks.filter((_, i) => i % 17 === 0);
  for (const offset of [72, 102]) {
    for (const h of picks) {
      scrollLineTo(leftView, h.leftStart, offset);
      await waitLayout(leftView);
      const open = await syncClosed(h.leftStart, h.rightStart);
      if (open >= 0) openLoop.push(open);
      const placed = placeRibbons();
      if (placed.count > 0) errorsRibbon.push(placed.maxError);
      const lTop = screenTopForLine(leftView, h.leftStart);
      const rTop = screenTopForLine(rightView, h.rightStart);
      if (lTop != null && rTop != null) errorsSync.push(Math.abs(lTop - rTop));
    }
  }
  if (openLoop.length) {
    const worst = Math.max(...openLoop);
    if (worst > 1) {
      // 记在样本名里不方便，调用方从 notes 读；这里把开环最差值塞进 name 的副作用避免改类型。
      (globalThis as { __open?: Record<string, number> }).__open ??= {};
      (globalThis as { __open: Record<string, number> }).__open[name] = Math.round(worst * 1000) / 1000;
    }
  }
  return {
    name,
    maxRibbonPx: errorsRibbon.length ? Math.max(...errorsRibbon) : -1,
    maxSyncPx: errorsSync.length ? Math.max(...errorsSync) : -1,
    samples: errorsSync.length,
  };
}

function foldRange(view: EditorView, fromLine: number, toLine: number) {
  if (toLine - fromLine < 30) return null;
  const from = view.state.doc.line(fromLine + 2).from;
  const to = view.state.doc.line(toLine - 2).to;
  if (to <= from) return null;
  return foldEffect.of({ from, to });
}

function foldUnchanged() {
  const leftEffects = [];
  const rightEffects = [];
  let prevL = 1;
  let prevR = 1;
  for (const h of hunks) {
    const l = foldRange(leftView, prevL, h.leftStart);
    const r = foldRange(rightView, prevR, h.rightStart);
    if (l) leftEffects.push(l);
    if (r) rightEffects.push(r);
    prevL = Math.max(h.leftEnd, h.leftStart);
    prevR = Math.max(h.rightEnd, h.rightStart);
  }
  if (leftEffects.length) leftView.dispatch({ effects: leftEffects });
  if (rightEffects.length) rightView.dispatch({ effects: rightEffects });
}

async function main() {
  const notes: string[] = [];
  (globalThis as { __open?: Record<string, number> }).__open = {};
  const withSyntax = new URLSearchParams(location.search).get("syntax") !== "0";
  statusEl.textContent = "正在生成 10 万行文档…";
  const tBuild0 = performance.now();
  const docs = buildDocs();
  const buildMs = performance.now() - tBuild0;
  hunks = docs.hunks;
  const leftBytes = new TextEncoder().encode(docs.left).length;
  const rightBytes = new TextEncoder().encode(docs.right).length;

  statusEl.textContent = "正在创建编辑器…";
  const tState0 = performance.now();
  const leftState = EditorState.create({
    doc: docs.left,
    extensions: baseExtensions(withSyntax),
  });
  const rightState = EditorState.create({
    doc: docs.right,
    extensions: baseExtensions(withSyntax),
  });
  const stateMs = performance.now() - tState0;

  const tView0 = performance.now();
  leftView = new EditorView({ state: leftState, parent: leftHost });
  rightView = new EditorView({ state: rightState, parent: rightHost });
  const viewMs = performance.now() - tView0;
  leftView.scrollDOM.addEventListener("scroll", () => syncScroll("left"), { passive: true });
  rightView.scrollDOM.addEventListener("scroll", () => syncScroll("right"), { passive: true });

  await waitLayout(leftView);
  const firstContentMs = performance.now() - tView0;
  let highlightClassMs: number | null = null;
  if (withSyntax) {
    const tH = performance.now();
    const deadline = performance.now() + 8000;
    while (performance.now() < deadline) {
      if (leftHost.querySelector(".cm-keyword, .tok-keyword, .ͼb, .ͼc, .ͼd")) {
        highlightClassMs = performance.now() - tH;
        break;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    if (highlightClassMs == null) {
      notes.push("8 秒内未观察到语法高亮 class，视口高亮可能尚未开始或选择器未命中。");
    }
  }

  anchors = buildAnchors(leftView, rightView, hunks);
  placeRibbons();
  // 先滚到文末再回到顶部，迫使高度表按真实行高稳定下来，避免估算高度导致滚动被夹住。
  leftView.scrollDOM.scrollTop = leftView.contentHeight;
  rightView.scrollDOM.scrollTop = rightView.contentHeight;
  await waitLayout(leftView);
  await waitLayout(rightView);
  leftView.scrollDOM.scrollTop = 0;
  syncScroll("left");
  await waitLayout(leftView);
  await waitLayout(rightView);
  notes.push(
    `contentHeight L=${Math.round(leftView.contentHeight)} R=${Math.round(rightView.contentHeight)} scrollHeight=${leftView.scrollDOM.scrollHeight}`,
  );

  const scenarios: Sample[] = [];
  scenarios.push(await measureScenario("scroll", async () => {
    leftView.scrollDOM.scrollTop = 0;
    syncScroll("left");
    await waitLayout(leftView);
  }));

  const tFold = performance.now();
  foldUnchanged();
  await waitLayout(leftView);
  await waitLayout(rightView);
  const foldMs = performance.now() - tFold;
  scenarios.push(await measureScenario("fold", async () => {}));

  scenarios.push(await measureScenario("font-16", async () => {
    leftView.dispatch({ effects: fontCompartment.reconfigure(theme("16px")) });
    rightView.dispatch({ effects: fontCompartment.reconfigure(theme("16px")) });
    await settle(leftView);
    await settle(rightView);
  }));

  scenarios.push(await measureScenario("font-20", async () => {
    leftView.dispatch({ effects: fontCompartment.reconfigure(theme("20px")) });
    rightView.dispatch({ effects: fontCompartment.reconfigure(theme("20px")) });
    await settle(leftView);
    await settle(rightView);
  }));

  const resizeRoot = document.querySelector<HTMLElement>("#compare")!;
  scenarios.push(await measureScenario("resize-narrow", async () => {
    resizeRoot.style.height = "420px";
    leftView.requestMeasure();
    rightView.requestMeasure();
    await waitLayout(leftView);
    await waitLayout(rightView);
  }));

  scenarios.push(await measureScenario("zoom-125", async () => {
    document.documentElement.style.zoom = "1.25";
    await waitLayout(leftView);
    await waitLayout(rightView);
  }));

  scenarios.push(await measureScenario("zoom-150", async () => {
    document.documentElement.style.zoom = "1.5";
    await waitLayout(leftView);
    await waitLayout(rightView);
  }));

  scenarios.push(await measureScenario("zoom-200", async () => {
    document.documentElement.style.zoom = "2";
    await waitLayout(leftView);
    await waitLayout(rightView);
  }));

  document.documentElement.style.zoom = "1";

  const open = (globalThis as { __open?: Record<string, number> }).__open;
  if (open && Object.keys(open).length) {
    notes.push(`openLoopMaxPx ${JSON.stringify(open)}`);
  }
  const finite = scenarios.flatMap((s) => [s.maxRibbonPx, s.maxSyncPx]).filter((n) => n >= 0);
  const maxErrorPx = finite.length ? Math.max(...finite) : 999;
  const pass = maxErrorPx <= 1;

  const results: SpikeResults = {
    generatedAt: new Date().toISOString(),
    userAgent: navigator.userAgent,
    doc: {
      leftLines: leftView.state.doc.lines,
      rightLines: rightView.state.doc.lines,
      leftBytes,
      rightBytes,
      hunks: hunks.length,
      pureInserts: hunks.filter((h) => h.leftEnd === h.leftStart).length,
    },
    open: {
      buildMs: round(buildMs),
      stateMs: round(stateMs),
      viewMs: round(viewMs),
      firstContentMs: round(firstContentMs),
      highlightClassMs: highlightClassMs == null ? null : round(highlightClassMs),
      withSyntax,
    },
    scrollHandler: {
      p95Ms: round(percentile(scrollCosts, 95)),
      maxMs: round(scrollCosts.length ? Math.max(...scrollCosts) : 0),
      n: scrollCosts.length,
    },
    foldMs: round(foldMs),
    scenarios,
    maxErrorPx: round(maxErrorPx),
    pass,
    notes,
  };
  window.__SPIKE_RESULTS__ = results;
  window.__SPIKE_DONE__ = true;
  const log = document.querySelector<HTMLElement>("#log")!;
  log.style.display = "block";
  log.textContent = JSON.stringify(results, null, 2);
  statusEl.textContent = pass
    ? `通过 · 最大误差 ${results.maxErrorPx}px`
    : `未通过 · 最大误差 ${results.maxErrorPx}px`;
}

function round(n: number): number {
  return Math.round(n * 1000) / 1000;
}

void main();
