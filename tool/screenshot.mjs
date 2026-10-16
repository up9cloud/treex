// Retakes assets/screenshot.png (the browser) and assets/screenshot-tui.png
// (the terminal): this checkout with Cargo.toml open, from a real treex. The
// terminal is captured from tmux and drawn by the same browser, so the only
// requirements here are tmux and playwright-core — no browser on this machine.
//
//   NODE_PATH=<dir>/node_modules node tool/screenshot.mjs <cdp-url> <this-host>
//
// <this-host> is this machine's address as the browser sees it.
import { createRequire } from "node:module";
import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";

// A CommonJS require, because that is the one that honors NODE_PATH.
const { chromium } = createRequire(import.meta.url)("playwright-core");

const [cdp, host] = process.argv.slice(2);
if (!cdp || !host) {
  console.error("usage: node tool/screenshot.mjs <cdp-url> <this-host>");
  process.exit(2);
}
const BIN = process.env.TREEX_BIN ?? "target/debug/treex";
const ROOT = process.cwd();
const FILE = `${ROOT}/Cargo.toml`;
// Off the default port, so a dev server already there is never the one told
// to open a file.
const port = () => 11800 + Math.floor(Math.random() * 100);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Its own tmux server: the default one is somebody's working session.
const tmux = (...args) =>
  execFileSync("tmux", ["-L", "treex-shot", "-f", "/dev/null", ...args], { encoding: "utf8" });

async function until(what, ok) {
  for (let i = 0; i < 100; i++) {
    if (await ok().catch(() => false)) return;
    await sleep(100);
  }
  throw new Error(`timed out waiting for ${what}`);
}

/// Opens Cargo.toml in the session behind `port`, as a click would.
async function open(port) {
  await until(`treex on ${port}`, () => fetch(`http://127.0.0.1:${port}/`).then((r) => r.ok));
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`);
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail; });
  ws.send(JSON.stringify({ type: "view", path: FILE }));
  await sleep(300);
  ws.close();
}

// -- ANSI from `tmux capture-pane -e` to HTML ------------------------------

const BASIC = ["#1e2127", "#e06c75", "#98c379", "#d19a66", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
               "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff"];
const BG = "#0d1117", FG = "#e6edf3";

function xterm(n) {
  if (n < 16) return BASIC[n];
  if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
  const c = n - 16, level = (v) => (v ? 55 + v * 40 : 0);
  return `rgb(${level(Math.floor(c / 36))},${level(Math.floor(c / 6) % 6)},${level(c % 6)})`;
}

/// Cells a character takes, near enough for what treex draws.
const wide = (cp) => (cp >= 0x1100 && cp <= 0x115f) || (cp >= 0x2e80 && cp <= 0xa4cf) ||
  (cp >= 0xac00 && cp <= 0xd7a3) || (cp >= 0xf900 && cp <= 0xfaff) || (cp >= 0xfe30 && cp <= 0xfe4f) ||
  (cp >= 0xff00 && cp <= 0xff60) || (cp >= 0xffe0 && cp <= 0xffe6) || (cp >= 0x1f300 && cp <= 0x1faff) ||
  cp >= 0x20000;

function toHtml(ansi) {
  // Outside ASCII a glyph can come from a fallback font of another width,
  // which shifts the rest of its row; each one is held to its cells.
  const esc = (s) => [...s].map((c) => {
    if (c === "&") return "&amp;";
    if (c === "<") return "&lt;";
    if (c === ">") return "&gt;";
    const cp = c.codePointAt(0);
    if (cp < 0x80) return c;
    return `<span style="display:inline-block;width:${wide(cp) ? 2 : 1}ch;text-align:center">${c}</span>`;
  }).join("");
  let st = {};
  const sgr = (codes) => {
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0) st = {};
      else if (c === 1) st.bold = true;
      else if (c === 2) st.dim = true;
      else if (c === 3) st.italic = true;
      else if (c === 4) st.underline = true;
      else if (c === 7) st.reverse = true;
      else if (c === 22) st.bold = st.dim = false;
      else if (c === 23) st.italic = false;
      else if (c === 24) st.underline = false;
      else if (c === 27) st.reverse = false;
      else if (c >= 30 && c <= 37) st.fg = BASIC[c - 30];
      else if (c >= 90 && c <= 97) st.fg = BASIC[c - 82];
      else if (c >= 40 && c <= 47) st.bg = BASIC[c - 40];
      else if (c >= 100 && c <= 107) st.bg = BASIC[c - 92];
      else if (c === 39) st.fg = null;
      else if (c === 49) st.bg = null;
      else if (c === 38 || c === 48) {
        const color = codes[i + 1] === 5 ? xterm(codes[i + 2])
          : `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`;
        i += codes[i + 1] === 5 ? 2 : 4;
        st[c === 38 ? "fg" : "bg"] = color;
      }
    }
  };
  const style = () => {
    let fg = st.fg ?? FG, bg = st.bg ?? null;
    if (st.reverse) [fg, bg] = [bg ?? BG, fg];
    return [`color:${fg}`, bg && `background:${bg}`, st.bold && "font-weight:bold",
            st.dim && "opacity:.6", st.italic && "font-style:italic",
            st.underline && "text-decoration:underline"].filter(Boolean).join(";");
  };
  return ansi.replace(/\n$/, "").split("\n").map((line) => {
    let out = "";
    for (const part of line.split(/(\x1b\[[0-9;]*m)/)) {
      const m = part.match(/^\x1b\[([0-9;]*)m$/);
      if (m) sgr(m[1] === "" ? [0] : m[1].split(";").map(Number));
      else if (part) out += `<span style="${style()}">${esc(part)}</span>`;
    }
    return out;
  }).join("\n");
}

// -- the two pictures --------------------------------------------------------

const browser = await chromium.connectOverCDP(cdp);
const context = await browser.newContext({
  viewport: { width: 720, height: 520 },
  deviceScaleFactor: 2,
  colorScheme: "dark",
});
const page = await context.newPage();

const web = port();
const server = spawn(BIN, ["--web", `0.0.0.0:${web}`, "--no-tui", ROOT], { stdio: "ignore" });
try {
  await open(web);
  await page.goto(`http://${host}:${web}/`);
  await page.waitForFunction(() => document.getElementById("text")?.textContent.includes("[package]"));
  await sleep(500);
  await page.screenshot({ path: "assets/screenshot.png" });
} finally {
  server.kill();
}

// Serving too, so it can be told what to open and the footer shows the page
// it shares — loopback, so the address is the one an installed treex shows.
const tui = port();
tmux("new-session", "-d", "-s", "shot", "-x", "124", "-y", "34",
     `${BIN} --web 127.0.0.1:${tui} ${ROOT}`);
let ansi;
try {
  await open(tui);
  await sleep(1000);
  ansi = tmux("capture-pane", "-e", "-p", "-t", "shot");
} finally {
  tmux("kill-session", "-t", "shot");
}
await page.setViewportSize({ width: 1000, height: 700 });
await page.setContent(`<!doctype html><body style="margin:0;background:${BG}">
  <pre id="t" style="display:inline-block;margin:0;padding:10px 12px;background:${BG};color:${FG};
       font:13px/1.16 Menlo,monospace">${toHtml(ansi)}</pre>`);
await page.locator("#t").screenshot({ path: "assets/screenshot-tui.png" });

await context.close();
await browser.close();
for (const f of ["assets/screenshot.png", "assets/screenshot-tui.png"]) {
  console.log(`${f}  ${(fs.statSync(f).size / 1024).toFixed(0)} KB`);
}
