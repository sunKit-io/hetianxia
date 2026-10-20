import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("pnpm build output is the app shell, not the one-line placeholder", () => {
  const html = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");
  const placeholder = readFileSync(new URL("../src-tauri/ui/index.html", import.meta.url), "utf8");
  const conf = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8")) as {
    build: { frontendDist: string; beforeBuildCommand: { script: string; cwd: string } };
  };
  assert.equal(conf.build.frontendDist, "../dist");
  assert.deepEqual(conf.build.beforeBuildCommand, { script: "pnpm build", cwd: ".." });
  assert.match(html, /id="root"/);
  assert.match(html, /assets\/index-/);
  assert.equal(html.includes("<p>合天下</p>"), false);
  assert.match(placeholder, /<p>合天下<\/p>/);
  assert.equal(html.includes(placeholder.trim()), false);
});
