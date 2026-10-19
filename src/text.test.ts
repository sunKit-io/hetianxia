import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Text } from "./plain.ts";

const hostile = `notes/<img src=x onerror=alert(1)>.txt & <script>alert("name")</script>`;

test("file paths, names, and contents render as text", () => {
  const html = renderToStaticMarkup(createElement(Text, { value: hostile }));
  assert.equal(html.includes("<img"), false);
  assert.equal(html.includes("<script"), false);
  assert.ok(html.includes("&lt;img"));
  assert.ok(html.includes("&lt;script"));
  assert.ok(html.includes("&amp;"));
  assert.ok(html.includes("notes/"));
});

test("the editor and screens do not inject HTML", () => {
  for (const file of ["App.tsx", "editor.tsx", "plain.ts"]) {
    const src = readFileSync(new URL(`./${file}`, import.meta.url), "utf8");
    assert.equal(src.includes("dangerouslySetInnerHTML"), false);
    assert.equal(/\.innerHTML\s*=/.test(src), false);
  }
  const editor = readFileSync(new URL("./editor.tsx", import.meta.url), "utf8");
  assert.match(editor, /EditorState\.create\(\{ doc: LEFT/);
});
