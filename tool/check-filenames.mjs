// Windows refuses to check out a path containing < > : " | ? *, a reserved
// device name, or a trailing dot or space. One such file in the repository
// makes `git clone` fail for every Windows user — and the failure arrives
// during checkout, before any job has run and with nothing useful to point at.
import { readdirSync } from "node:fs";
import { join } from "node:path";

const ILLEGAL = ["<", ">", ":", '"', "|", "?", "*"];
const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/i;
const SKIP = new Set(["target", ".git", "node_modules"]);

const problems = [];

function walk(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (SKIP.has(entry.name)) continue;
    const path = join(dir, entry.name);
    const why = [];

    const bad = ILLEGAL.filter((c) => entry.name.includes(c));
    if (bad.length) why.push(`Windows forbids ${bad.map((c) => `'${c}'`).join(" ")}`);
    if (RESERVED.test(entry.name)) why.push("reserved device name");
    if (/[. ]$/.test(entry.name)) why.push("trailing dot or space");

    if (why.length) problems.push(`${path} — ${why.join("; ")}`);
    if (entry.isDirectory()) walk(path);
  }
}

walk(".");

if (problems.length) {
  console.error("These paths cannot be checked out on Windows:\n");
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}
console.log("every path is checkout-safe on Windows");
