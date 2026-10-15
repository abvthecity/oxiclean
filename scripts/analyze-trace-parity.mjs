// Compares the JSON output of `analyze-trace --json` with that of
// `oxiclean trace --format json` on the same trace:
//
//   node scripts/analyze-trace-parity.mjs analyze-trace.json oxiclean.json ROOT [TOLERANCE_MS]
//
// Hot spots are compared as chains of frames from a root frame down, each
// frame named by its kind, file and position, so a frame that moved to
// another parent counts as missing on one side and extra on the other.
// Times may differ by TOLERANCE_MS (default 1) for rounding. Packages
// analyze-trace finds loaded from more than one directory must be found by
// oxiclean too, from the same directories; oxiclean may find more, as it
// also reads createSourceFile events. Exits 1 on any difference.

import fs from "node:fs";
import path from "node:path";

const [atFile, oxFile, rootArg, tolArg] = process.argv.slice(2);
if (!rootArg) {
  console.error("usage: node analyze-trace-parity.mjs analyze-trace.json oxiclean.json ROOT [TOLERANCE_MS]");
  process.exit(2);
}
const tolerance = Number(tolArg ?? 1);
const root = fs.realpathSync(rootArg);

/** A path relative to the root with `/`, or absolute when outside it. */
function norm(p) {
  let abs = path.isAbsolute(p) ? p : path.join(root, p);
  try {
    abs = fs.realpathSync(abs);
  } catch {
    abs = path.normalize(abs);
  }
  const rel = path.relative(root, abs);
  return rel.startsWith("..") || path.isAbsolute(rel) ? abs : rel.split(path.sep).join("/");
}

// analyze-trace: a tree per project.
const at = JSON.parse(fs.readFileSync(atFile, "utf8"));
const atChains = new Map();
const atDuplicates = new Map();
for (const result of at.results ?? []) {
  const h = result.highlights ?? {};
  const walk = (node, prefix) => {
    let name = node.description;
    if (name.startsWith("Check file ") || name === "Emit declarations file") {
      name = `${name === "Emit declarations file" ? name : "Check file"} ${norm(node.path)}`;
    } else if (node.startLine !== undefined) {
      name += ` ${norm(node.path)}:${node.startLine}:${node.startChar}-${node.endLine}:${node.endChar}`;
    } else if (node.startOffset !== undefined) {
      name += ` ${norm(node.path)}@${node.startOffset}-${node.endOffset}`;
    }
    const chain = prefix ? `${prefix} > ${name}` : name;
    const times = atChains.get(chain) ?? [];
    times.push(node.timeMs);
    atChains.set(chain, times);
    for (const c of node.children ?? []) walk(c, chain);
  };
  for (const n of h.hotSpots ?? []) walk(n, "");
  for (const d of h.duplicatePackages ?? []) {
    const dirs = d.instances.map((i) => norm(i.path)).sort();
    atDuplicates.set(d.name, dirs);
  }
}

// oxiclean: findings, each naming its parent in an "inside ..." entry.
const ox = JSON.parse(fs.readFileSync(oxFile, "utf8"));
const hot = ox.findings.filter((f) => f.rule === "hot-spots");
const parsed = hot.map((f) => {
  const [, description, ms] = /^(.*?) took (\d+)ms/.exec(f.message);
  let name = description;
  let m;
  if ((m = /^Check file (.+)$/.exec(description))) name = `Check file ${norm(m[1])}`;
  else if ((m = /^Emit declarations file (.+)$/.exec(description))) name = `Emit declarations file ${norm(m[1])}`;
  else if ((m = /^(.+?)(?: in (.+?))? from \(line (\d+), char (\d+)\) to \(line (\d+), char (\d+)\)$/.exec(description)))
    name = `${m[1]} ${norm(m[2] ?? f.file)}:${m[3]}:${m[4]}-${m[5]}:${m[6]}`;
  else if ((m = /^(.+?) in (.+) from offset (\d+) to offset (\d+)$/.exec(description)))
    name = `${m[1]} ${norm(m[2])}@${m[3]}-${m[4]}`;
  const inside = f.related.find((r) => r.message.startsWith("inside "));
  return { f, description, name, ms: Number(ms), parent: inside && { file: inside.file, description: inside.message.slice(7) } };
});
const oxChains = new Map();
const chainOf = (p, seen = new Set()) => {
  if (!p.parent || seen.has(p)) return p.name;
  seen.add(p);
  const parent = parsed.find((q) => q.f.file === p.parent.file && q.description === p.parent.description);
  return parent ? `${chainOf(parent, seen)} > ${p.name}` : `? > ${p.name}`;
};
for (const p of parsed) {
  const chain = chainOf(p);
  const times = oxChains.get(chain) ?? [];
  times.push(p.ms);
  oxChains.set(chain, times);
}
const oxDuplicates = new Map();
for (const f of ox.findings.filter((f) => f.rule === "duplicate-packages")) {
  const name = /^tsc loads `([^`]+)`/.exec(f.message)[1];
  const list = f.message.slice(f.message.indexOf("directories: ") + 13).split(";")[0];
  const dirs = [...list.matchAll(/ at ([^,]+)/g)].map((m) => norm(m[1].trim())).sort();
  oxDuplicates.set(name, dirs);
}

// Compare.
const problems = [];
let matched = 0;
for (const [chain, times] of atChains) {
  const other = oxChains.get(chain);
  if (!other) {
    problems.push(`only analyze-trace: ${chain} (${times.join(", ")}ms)`);
    continue;
  }
  if (other.length !== times.length) problems.push(`analyze-trace has ${times.length}, oxiclean ${other.length}: ${chain}`);
  const a = [...times].sort((x, y) => x - y);
  const b = [...other].sort((x, y) => x - y);
  a.forEach((t, i) => {
    if (b[i] !== undefined && Math.abs(t - b[i]) > tolerance) problems.push(`time ${t}ms vs ${b[i]}ms: ${chain}`);
  });
  matched += Math.min(a.length, b.length);
}
for (const [chain, times] of oxChains) {
  if (!atChains.has(chain)) problems.push(`only oxiclean: ${chain} (${times.join(", ")}ms)`);
}
let dupMatched = 0;
for (const [name, dirs] of atDuplicates) {
  const other = oxDuplicates.get(name);
  if (!other) problems.push(`duplicate package only analyze-trace: ${name} (${dirs.join(", ")})`);
  else if (other.join() !== dirs.join()) problems.push(`duplicate package ${name}: ${dirs.join(", ")} vs ${other.join(", ")}`);
  else dupMatched++;
}
const extra = [...oxDuplicates.keys()].filter((n) => !atDuplicates.has(n));

console.log(
  `hot spots: ${matched} frames match of ${[...atChains.values()].flat().length} (analyze-trace) and ${parsed.length} (oxiclean); ` +
    `duplicate packages: ${dupMatched} of ${atDuplicates.size} match` +
    (extra.length ? `, oxiclean also finds ${extra.join(", ")}` : ""),
);
for (const p of problems) console.log(`  ${p}`);
process.exit(problems.length ? 1 : 0);
