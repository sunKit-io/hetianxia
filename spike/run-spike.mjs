import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const here = path.dirname(fileURLToPath(import.meta.url));
const chrome = process.env.CHROME_PATH || "/usr/bin/google-chrome";
const port = 4178;

function startPreview() {
  const child = spawn(
    "pnpm",
    ["exec", "vite", "preview", "--port", String(port), "--host", "127.0.0.1", "--strictPort"],
    { cwd: here, stdio: ["ignore", "pipe", "pipe"] },
  );
  let buf = "";
  child.stdout.on("data", (d) => {
    buf += d.toString();
  });
  child.stderr.on("data", (d) => {
    buf += d.toString();
  });
  return { child, log: () => buf };
}

async function waitForServer() {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/`);
      if (res.ok) return;
    } catch {
      /* retry */
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error("vite preview did not start");
}

async function runOnce(browser, syntax) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 800, deviceScaleFactor: 1 });
  page.setDefaultTimeout(120000);
  await page.goto(`http://127.0.0.1:${port}/?syntax=${syntax}`, { waitUntil: "domcontentloaded" });
  await page.waitForFunction("window.__SPIKE_DONE__ === true", { timeout: 120000 });
  const results = await page.evaluate(() => window.__SPIKE_RESULTS__);
  await page.close();
  return results;
}

const preview = startPreview();
try {
  await waitForServer();
  const browser = await puppeteer.launch({
    executablePath: chrome,
    headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--font-render-hinting=none"],
  });
  try {
    const withSyntax = await runOnce(browser, "1");
    const noSyntax = await runOnce(browser, "0");
    const out = { withSyntax, noSyntax };
    await mkdir(path.join(here, "out"), { recursive: true });
    const file = path.join(here, "out", "results.json");
    await writeFile(file, JSON.stringify(out, null, 2));
    console.log(JSON.stringify(out, null, 2));
    const pass = withSyntax.pass && noSyntax.pass;
    if (!pass) process.exitCode = 2;
  } finally {
    await browser.close();
  }
} catch (err) {
  console.error(err);
  console.error(preview.log());
  process.exitCode = 1;
} finally {
  preview.child.kill("SIGTERM");
}
