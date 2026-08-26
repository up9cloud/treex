// Runs the real script out of index.html against a real treex server, in a
// minimal DOM. It exists because the page can break silently: a renamed field
// arrives as `undefined` and every row quietly becomes a non-directory, which
// no Rust test would notice.
import { spawn } from "node:child_process";
import fs from "node:fs";
import vm from "node:vm";

const BIN = process.argv[2] ?? "target/debug/treex";
const PORT = 11790 + Math.floor(Math.random() * 100);
const ORIGIN = `http://localhost:${PORT}`;

const fixture = fs.mkdtempSync("/tmp/treex-page-");
fs.mkdirSync(`${fixture}/alpha/nested`, { recursive: true });
fs.writeFileSync(`${fixture}/alpha/inner.txt`, "x");
fs.writeFileSync(`${fixture}/top.txt`, "hello\nfrom treex\n");
fs.writeFileSync(`${fixture}/.hidden`, "x");
fs.writeFileSync(`${fixture}/od#d?name.txt`, "awkward name");
fs.writeFileSync(`${fixture}/big.txt`, "x".repeat(5000));
fs.writeFileSync(`${fixture}/blob.bin`, Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0, 0, 0, 1]));

const server = spawn(BIN, ["--web", String(PORT), "--no-tui", "--max-preview-size", "2k", fixture],
                     { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let failures = 0;
const check = (ok, what, detail = "") => {
  console.log(`${ok ? "  ok  " : "FAIL  "}${what}${detail && !ok ? ` — ${detail}` : ""}`);
  if (!ok) failures++;
};

class El {
  constructor(tag) {
    this.href = "";
    this.tag = tag; this.children = []; this.className = ""; this._text = "";
    this.onclick = null; this.disabled = false; this.hidden = false;
    this.scrollTop = 0; this.scrollHeight = 1000; this.clientHeight = 100;   // small enough that the fixture overflows it
    this.offsetHeight = 22; this.title = ""; this.style = {};
    this.id = "";

    this.classList = {
      add: (c) => { if (!this.className.includes(c)) this.className += ` ${c}`; },
      remove: (c) => { this.className = this.className.replace(c, "").trim(); },
      toggle: (c) => this.className.includes(c) ? this.classList.remove(c) : this.classList.add(c),
      contains: (c) => this.className.includes(c),
    };
  }
  // The real DOM stringifies whatever you assign; matching that here keeps
  // the harness from disagreeing with the browser over numbers.
  set textContent(v) { this._text = String(v); this.children = []; }
  get textContent() { return this._text || this.children.map((c) => c.textContent).join(""); }
  appendChild(c) { this.children.push(c); }
  remove() { }
  addEventListener() { }
  scrollIntoView() { this.scrolled = (this.scrolled ?? 0) + 1; }
  replaceChildren(...nodes) {
    this.children = nodes.length === 1 && nodes[0]?.tag === "#frag" ? [...nodes[0].children] : nodes;
  }
}

// Unknown ids are created on demand, so adding an element to the page does not
// mean editing this file.
const ids = new Proxy({}, {
  get(store, id) {
    if (!(id in store)) { store[id] = new El("div"); store[id].id = id; }
    return store[id];
  },
});

const vars = {};
let fetches = 0;
const storage = new Map();
const sent = [];
let ws0;
const script = fs.readFileSync("src/web/assets/index.html", "utf8").match(/<script>([\s\S]*?)<\/script>/)[1];
const ctx = {
  WebSocket: class extends WebSocket {
    constructor(...a) { super(...a); ws0 = this; }
    send(data) { sent.push(JSON.parse(data)); super.send(data); }
  },
  // The page uses same-origin paths; node's fetch needs them absolute.
  fetch: (url, init) => { fetches++; return fetch(new URL(url, ORIGIN), init); },
  localStorage: {
    getItem: (k) => (storage.has(k) ? storage.get(k) : null),
    setItem: (k, v) => storage.set(k, String(v)),
  },
  document: {
    documentElement: { style: { setProperty: (k, v) => { vars[k] = v; } } },
    getElementById: (id) => ids[id],
    createElement: (t) => new El(t),
    createDocumentFragment: () => new El("#frag"),
    querySelectorAll: () => [],
    addEventListener: () => {},
  },
  location: { protocol: "http:", host: `localhost:${PORT}` },
  // The page reads its starting font size out of the stylesheet.
  getComputedStyle: () => ({ getPropertyValue: (k) => (k === "--fs" ? "10px" : "") }),
  setTimeout, console, JSON, URL, encodeURIComponent, Promise, Object,
};

// Rows now live inside the spacer, and only the visible window exists.
const rows = () => ids.spacer.children.map((el) => el.textContent);
const rowFor = (name) => ids.spacer.children.find((el) => el.textContent.includes(name));
const viewerText = () => ids.vbody.children.map((c) => c.textContent).join("");
const lastSent = (type) => [...sent].reverse().find((c) => c.type === type);

try {
  await sleep(1200);
  vm.createContext(ctx);
  vm.runInContext(script, ctx);
  await sleep(700);

  check(rows().length > 0, "the page renders rows", "nothing rendered — is the server up?");
  check(rows().some((r) => r.includes("▸")), "directories render a collapsed marker",
        `isDir is not reaching the page: ${JSON.stringify(rows())}`);
  // The bidi marks around it are deliberate; the path between them is not.
  check(ids.root.textContent.replace(/[\u202a\u202c]/g, "") === fixture,
        "the header shows the root path intact", JSON.stringify(ids.root.textContent));

  // Dotfiles are shown by default, and the button is the same switch as `.`.
  check(rows().some((r) => r.includes(".hidden")), "dotfiles show by default",
        JSON.stringify(rows()));
  check(ids.hidden.className === "", "so the hide filter starts unlit", ids.hidden.className);
  ids.hidden.onclick();
  await sleep(600);
  check(!rows().some((r) => r.includes(".hidden")), "the button hides them",
        JSON.stringify(rows()));
  check(ids.hidden.className === "on", "and lights up while it is hiding them",
        ids.hidden.className);
  ids.hidden.onclick();
  await sleep(600);
  check(rows().some((r) => r.includes(".hidden")), "and brings them back");
  check(ids.hidden.className === "", "and goes unlit again", ids.hidden.className);

  const alpha = rowFor("alpha");
  check(!!alpha && alpha.className.includes("kind-d"), "a directory row is classed as one");

  // The bug this file exists for: one click, one expansion.
  sent.length = 0;
  const before = rows().length;
  alpha.onclick();
  await sleep(400);
  check(sent.length === 1 && sent[0].type === "toggle",
        "clicking a directory sends exactly one toggle", JSON.stringify(sent));
  check(rows().length > before, "one click expands it",
        `still ${rows().length} rows: ${JSON.stringify(rows())}`);
  check(rows().some((r) => r.includes("inner.txt")), "the children are now visible");
  check(rowFor("alpha").textContent.includes("▾"), "the marker flipped to expanded");

  // And clicking again collapses, as in VS Code.
  rowFor("alpha").onclick();
  await sleep(400);
  check(!rows().some((r) => r.includes("inner.txt")), "clicking again collapses it");

  // A cursor move must not rebuild the tree: the same elements stay in place.
  const beforeEls = [...ids.spacer.children];
  const beforeSel = beforeEls.findIndex((e) => e.className.includes("sel"));
  sent.length = 0;
  ws0.send(JSON.stringify({ type: "moveSelection", delta: 1 }));
  await sleep(600);
  const afterEls = [...ids.spacer.children];
  check(afterEls.length === beforeEls.length && afterEls.every((e, i) => e === beforeEls[i]),
        "a cursor move reuses the row elements instead of rebuilding");
  const afterSel = afterEls.findIndex((e) => e.className.includes("sel"));
  check(afterSel === beforeSel + 1, "and the highlight moved by one",
        `${beforeSel} -> ${afterSel}`);
  // scrollIntoView cannot be used any more: the target row may not exist yet.
  // Reveal works from the index, so check the arithmetic instead.
  const rowH = ids.spacer.children[0].offsetHeight || 22;
  for (let i = 0; i < 40; i++) ws0.send(JSON.stringify({ type: "moveSelection", delta: 1 }));
  await sleep(700);
  const sel = ids.spacer.children.findIndex((e) => e.className.includes("sel"));
  check(sel >= 0, "the cursor stays rendered after moving far down",
        `scrollTop=${ids.tree.scrollTop}`);
  check(ids.tree.scrollTop > 0, "and the pane scrolled to keep it in view",
        `scrollTop=${ids.tree.scrollTop}`);
  ws0.send(JSON.stringify({ type: "selectRow", row: 0 }));
  await sleep(500);
  check(ids.tree.scrollTop === 0, "and back to the top when the cursor returns");

  // A file selects rather than toggles; the viewer follows from the snapshot.
  sent.length = 0;
  rowFor("top.txt").onclick();
  await sleep(800);
  check(sent.length === 1 && sent[0].type === "view",
        "clicking a file in the browser goes straight to reading it", JSON.stringify(sent));

  check(ids.viewer.hidden === false, "the viewer opens");
  check(ids.vname.textContent === "top.txt", "the viewer names the file", ids.vname.textContent);
  check(ids.vraw.href === "/f/top.txt", "the new-tab link points at the file's own URL",
        ids.vraw.href);
  check(ids.vraw.hidden === false, "and the floating button appears with the viewer");

  // `#` and `?` mean something in a URL; encodeURI leaves them alone.
  ids.vclose.onclick();
  rowFor("od#d?name.txt").onclick();
  await sleep(800);
  check(ids.vraw.href === "/f/od%23d%3Fname.txt",
        "awkward characters in a name are percent-encoded", ids.vraw.href);
  const reachable = await ctx.fetch(ids.vraw.href).then((r) => r.status);
  check(reachable === 200, "and the link actually resolves", String(reachable));
  ids.vclose.onclick();
  rowFor("top.txt").onclick();
  await sleep(800);
  check(ids.text.textContent.includes("hello\nfrom treex"), "the contents are shown",
        ids.text.textContent);

  // Line numbers, on by default.
  check(ids.gutter.hidden === false, "line numbers are on by default");
  check(ids.gutter.textContent.split("\n")[0] === "1", "the gutter starts at 1",
        JSON.stringify(ids.gutter.textContent));
  check(ids.gutter.textContent.split("\n").length ===
          ids.text.textContent.replace(/\n$/, "").split("\n").length,
        "the gutter has one number per line, and no phantom for the trailing newline",
        `${ids.gutter.textContent.split("\n").length} numbers vs ${ids.text.textContent}`);
  ids.vlines.onclick();
  check(ids.gutter.hidden === true, "the # button hides them");
  ids.vlines.onclick();
  check(ids.gutter.hidden === false, "and brings them back");

  // Font size: half-point steps from a default of 10, and content only.
  check(ids.fsnow.textContent === "10", "the font size starts at 10", ids.fsnow.textContent);
  ids.fsup.onclick();
  check(ids.fsnow.textContent === "10.5", "+ steps by 0.5", ids.fsnow.textContent);
  check(vars["--fs"] === "10.5px", "and it reaches the page", vars["--fs"]);
  check(ids.vfsnow.textContent === ids.fsnow.textContent, "both navs show the same size");
  ids.fsup.onclick();
  check(ids.fsnow.textContent === "11", "whole numbers lose the .0", ids.fsnow.textContent);
  ids.fsdown.onclick(); ids.fsdown.onclick();
  check(ids.fsnow.textContent === "10", "- steps back down");
  check(vars["--ui"] === undefined, "the nav's own size is never touched", String(vars["--ui"]));

  // Scroll buttons act on whichever pane is showing.
  ids.vbody.scrollTop = 40;
  ids.bottom.onclick();
  check(ids.vbody.scrollTop === ids.vbody.scrollHeight, "scroll-to-bottom targets the viewer");
  ids.top.onclick();
  check(ids.vbody.scrollTop === 0, "scroll-to-top targets the viewer");

  ids.vbody.scrollTop = 0;
  ids.pgdn.onclick();
  check(ids.vbody.scrollTop === Math.round(ids.vbody.clientHeight * 0.9),
        "page down moves just under one screen", String(ids.vbody.scrollTop));
  ids.pgup.onclick();
  check(ids.vbody.scrollTop === 0, "page up comes back");
  ids.pgup.onclick();
  check(ids.vbody.scrollTop === 0, "and never goes negative");

  sent.length = 0;
  ids.vclose.onclick();
  check(ids.viewer.hidden === true, "back closes the viewer");
  check(ids.vraw.hidden === true, "and the floating button goes with it");
  check(lastSent("view") && lastSent("view").path === null,
        "and tells the session nothing is open", JSON.stringify(sent));

  ids.tree.scrollTop = 40;
  ids.top.onclick();
  check(ids.tree.scrollTop === 0, "with the viewer closed they target the tree");

  rowFor("big.txt").onclick();
  await sleep(800);
  check(viewerText().includes("preview limit"), "a file over the limit says so", viewerText());
  check(!viewerText().includes("xxxx"), "and its contents are not sent");

  ids.vclose.onclick();
  rowFor("blob.bin").onclick();
  await sleep(800);
  check(viewerText().includes("binary"), "a binary file is refused", viewerText());

  // Flipping back to a file already read must not go to the server again.
  const fetchesBefore = fetches;
  ids.vclose.onclick();
  rowFor("top.txt").onclick();
  await sleep(800);
  check(ids.text.textContent.includes("hello"), "a cached file still renders",
        ids.text.textContent);
  // One revalidation request is expected; the point is that it comes back
  // "unchanged" rather than carrying the file again.
  check(fetches === fetchesBefore + 1, "with exactly one revalidation",
        `${fetches - fetchesBefore} request(s)`);

  // The row being read is marked, and moving off it puts the file away.
  ids.vclose.onclick();
  rowFor("top.txt").onclick();
  await sleep(800);
  check(rowFor("top.txt").className.includes("viewing"),
        "the row being read is marked", rowFor("top.txt").className);
  rowFor("alpha").onclick();
  await sleep(800);
  check(ids.viewer.hidden === true, "moving onto a directory closes the viewer");
  check(!rowFor("top.txt").className.includes("viewing"),
        "and the mark goes with it", rowFor("top.txt").className);
} finally {
  server.kill();
  fs.rmSync(fixture, { recursive: true, force: true });
}

console.log(failures ? `\n${failures} failed` : "\nall page checks passed");
process.exit(failures ? 1 : 0);
