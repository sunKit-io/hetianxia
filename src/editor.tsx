import { useEffect, useMemo, useRef, useState } from "react";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import { bracketMatching, defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { EditorView, drawSelection, keymap, lineNumbers } from "@codemirror/view";
import { lineHunks, mapDocY, screenCorrection, type Anchor, type Hunk } from "./syncScroll";

const LEFT = sample("left");
const RIGHT = sample("right");

function sample(side: "left" | "right"): string {
  const lines: string[] = [];
  for (let i = 1; i <= 48; i++) lines.push(`export const keep${i} = ${i};`);
  lines[12] = side === "left" ? "export const port = 43127;" : "export const port = 8080;";
  if (side === "right") lines.splice(28, 0, "export const tracing = true;");
  return lines.join("\n");
}

function extensions() {
  return [
    lineNumbers(),
    drawSelection(),
    history(),
    keymap.of([...defaultKeymap, ...historyKeymap]),
    bracketMatching(),
    javascript(),
    syntaxHighlighting(defaultHighlightStyle),
    EditorView.editable.of(true),
    EditorView.theme({
      "&": { height: "480px", fontSize: "12px" },
      ".cm-scroller": { fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace', lineHeight: "1.45" },
      ".cm-gutters": { background: "#fafafa", borderRight: "1px solid #ececf0" },
    }),
  ];
}

function docTop(view: EditorView, lineNo: number): number {
  const max = view.state.doc.lines;
  const n = Math.min(Math.max(lineNo, 1), max);
  const line = view.state.doc.line(n);
  const scale = view.scaleY || 1;
  return view.lineBlockAt(line.from).top / scale;
}

function buildAnchors(left: EditorView, right: EditorView, hunks: Hunk[]): Anchor[] {
  const anchors: Anchor[] = [{ left: 0, right: 0 }];
  for (const hunk of hunks) {
    anchors.push({ left: docTop(left, hunk.leftStart), right: docTop(right, hunk.rightStart) });
    const leftEnd = Math.max(hunk.leftStart, hunk.leftEnd - 1);
    const rightEnd = Math.max(hunk.rightStart, hunk.rightEnd - 1);
    anchors.push({ left: docTop(left, leftEnd), right: docTop(right, rightEnd) });
  }
  const leftLast = left.state.doc.line(left.state.doc.lines);
  const rightLast = right.state.doc.line(right.state.doc.lines);
  const scaleL = left.scaleY || 1;
  const scaleR = right.scaleY || 1;
  anchors.push({
    left: left.lineBlockAt(leftLast.from).bottom / scaleL,
    right: right.lineBlockAt(rightLast.from).bottom / scaleR,
  });
  anchors.sort((a, b) => a.left - b.left || a.right - b.right);
  return anchors;
}

function screenTop(view: EditorView, lineNo: number): number | null {
  const max = view.state.doc.lines;
  const line = view.state.doc.line(Math.min(Math.max(lineNo, 1), max));
  return view.coordsAtPos(line.from)?.top ?? null;
}

export function DiffEditor({ onWriteFailed }: { onWriteFailed: () => void }) {
  const leftHost = useRef<HTMLDivElement>(null);
  const rightHost = useRef<HTMLDivElement>(null);
  const leftView = useRef<EditorView | null>(null);
  const rightView = useRef<EditorView | null>(null);
  const hunks = useMemo(() => lineHunks(LEFT, RIGHT), []);
  const [choice, setChoice] = useState<"left" | "right" | "both">("right");
  const [bothOrder, setBothOrder] = useState<"left-top" | "right-top">("left-top");
  const [active, setActive] = useState(0);

  useEffect(() => {
    if (!leftHost.current || !rightHost.current) return;
    const left = new EditorView({
      state: EditorState.create({ doc: LEFT, extensions: extensions() }),
      parent: leftHost.current,
    });
    const right = new EditorView({
      state: EditorState.create({ doc: RIGHT, extensions: extensions() }),
      parent: rightHost.current,
    });
    leftView.current = left;
    rightView.current = right;
    let syncing: "left" | "right" | "lock" | null = null;
    let anchors = buildAnchors(left, right, hunks);

    const sync = (from: "left" | "right") => {
      if (syncing) return;
      syncing = from;
      const src = from === "left" ? left : right;
      const dst = from === "left" ? right : left;
      const mapped = from === "left"
        ? mapDocY(anchors, src.scrollDOM.scrollTop)
        : mapDocY(anchors.map((a) => ({ left: a.right, right: a.left })), src.scrollDOM.scrollTop);
      if (Math.abs(dst.scrollDOM.scrollTop - mapped) > 0.25) dst.scrollDOM.scrollTop = mapped;
      const hunk = hunks[0];
      syncing = "lock";
      requestAnimationFrame(() => {
        if (hunk) {
          const l = screenTop(left, hunk.leftStart);
          const r = screenTop(right, hunk.rightStart);
          if (l != null && r != null && Math.abs(r - l) > 0.5) {
            const delta = screenCorrection(r, l, right.scaleY || 1);
            right.scrollDOM.scrollTop += delta;
          }
        }
        requestAnimationFrame(() => {
          syncing = null;
        });
      });
    };
    const onLeft = () => sync("left");
    const onRight = () => sync("right");
    left.scrollDOM.addEventListener("scroll", onLeft);
    right.scrollDOM.addEventListener("scroll", onRight);
    const refresh = () => {
      anchors = buildAnchors(left, right, hunks);
    };
    window.addEventListener("resize", refresh);
    return () => {
      left.scrollDOM.removeEventListener("scroll", onLeft);
      right.scrollDOM.removeEventListener("scroll", onRight);
      window.removeEventListener("resize", refresh);
      left.destroy();
      right.destroy();
      leftView.current = null;
      rightView.current = null;
    };
  }, [hunks]);

  const jump = (index: number) => {
    setActive(index);
    const hunk = hunks[index];
    const left = leftView.current;
    const right = rightView.current;
    if (!hunk || !left || !right) return;
    const line = left.state.doc.line(Math.min(hunk.leftStart, left.state.doc.lines));
    left.dispatch({ effects: EditorView.scrollIntoView(line.from, { y: "start" }) });
  };

  const choiceLabel = choice === "left" ? "保留左边" : choice === "both" ? (bothOrder === "left-top" ? "都保留，左边在上" : "都保留，右边在上") : "用右边";
  const reset = () => {
    setChoice("right");
    setBothOrder("left-top");
  };

  return (
    <div className="editorwrap" data-merge={choice} data-order={bothOrder}>
      <div className="filebar">
        <button className={choice === "right" ? "on" : ""} onClick={() => setChoice("right")}>全部用右边</button>
        <button className={choice === "left" ? "on" : ""} onClick={() => setChoice("left")}>全部保留左边</button>
        <button onClick={reset}>全部恢复</button>
      </div>
      <div className="editor">
        <div className="pane" ref={leftHost} data-pane="left" />
        <div className="gutterbar">
          <b>中间工具条</b>
          <button className={choice === "right" ? "on" : ""} onClick={() => setChoice("right")}>用右边</button>
          <button className={choice === "left" ? "on" : ""} onClick={() => setChoice("left")}>保留左边</button>
          <button className={choice === "both" ? "on" : ""} onClick={() => setChoice("both")}>都保留</button>
          <button onClick={reset}>恢复</button>
          {choice === "both" && (
            <div className="bothorder" role="group" aria-label="都保留的先后">
              <button className={bothOrder === "left-top" ? "on" : ""} onClick={() => setBothOrder("left-top")}>左边在上</button>
              <button className={bothOrder === "right-top" ? "on" : ""} onClick={() => setBothOrder("right-top")}>右边在上</button>
            </div>
          )}
        </div>
        <div className="pane" ref={rightHost} data-pane="right" />
      </div>
      <div className="difflist">
        <h2>差异列表</h2>
        <ul>
          {hunks.map((hunk, index) => (
            <li key={`${hunk.leftStart}-${hunk.rightStart}`}>
              <button className={index === active ? "on" : ""} onClick={() => jump(index)}>
                左 {hunk.leftStart}–{Math.max(hunk.leftStart, hunk.leftEnd - 1)} 行 · 右 {hunk.rightStart}–{Math.max(hunk.rightStart, hunk.rightEnd - 1)} 行
              </button>
              <em>{choiceLabel}</em>
            </li>
          ))}
        </ul>
        <footer>
          <span>OrderCenter · src/config/app.config.js</span>
          <button className="primary" onClick={onWriteFailed}>执行升级</button>
        </footer>
      </div>
    </div>
  );
}
