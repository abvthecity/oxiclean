// Regenerates the traces of the fixtures in this directory with a real tsc:
//
//   node crates/oxiclean_trace/tests/fixtures/regenerate.mjs path/to/tsc [path/to/tsc7]
//   node crates/oxiclean_trace/tests/fixtures/regenerate.mjs --raw OUT path/to/tsc [path/to/tsc7]
//   node crates/oxiclean_trace/tests/fixtures/regenerate.mjs --golden path/to/analyze-trace
//
// The tsgo fixture is traced with TypeScript 7 (the Go compiler), the
// others with the first tsc; without a second, the tsgo fixture is left as
// it is. FIXTURES=npm,tsgo picks which fixtures to regenerate.
//
// With `--raw OUT`, each fixture is set up in OUT/<name>/repo with its
// trace, untouched, in OUT/<name>/repo/trace, and the fixtures are left
// as they are; scripts/analyze-trace-parity.sh uses this.
//
// Each fixture is copied to a temporary directory, its package manager's
// symlinks are made there (they aren't committed, so the fixtures check
// out on Windows), and tsc writes a trace of it. The trace is then made
// small enough to commit: bind events and the parse events of TypeScript's
// lib files are dropped, types.json
// keeps only the types the trace names (the others become `null`, which
// keeps every type at its index), and absolute paths are rewritten as if
// the trace had been recorded on another machine, which oxiclean must
// cope with. The deno fixture is rewritten as a Windows trace, where tsc
// lowercases paths, and the tsgo fixture as a macOS trace, where it does
// too. The pnpm fixture is traced with `tsc --build`, so it has the
// legend.json layout, and keeps the declarations it emits. The tsgo
// fixture has TypeScript 7's layout: one trace.json whose checkers run on
// threads of their own, and a types_N.json per checker, which legend.json
// tells apart by `checkerId`.
//
// Timings differ from run to run, so the expected output in tests/fixtures.rs
// changes with every regeneration.
//
// With `--golden`, nothing is traced: analyze-trace (0.11.1) reads each
// fixture's committed trace, with the paths in it pointing at the fixture
// so it can read the sources, at the fixture's hot-spot settings, and its
// JSON output is written to <fixture>/analyze-trace.json with the fixture's
// path as `<root>` and the trace directory as `<trace>`. The parity test in
// src/hot_spots/parity.rs compares oxiclean's hot spots with it. Run it after
// regenerating the traces.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const here = path.dirname(new URL(import.meta.url).pathname);
const args = process.argv.slice(2);
const raw = args[0] === "--raw" ? path.resolve(args[1]) : undefined;
const tsc = raw ? args[2] : args[0];
const tsc7 = raw ? args[3] : args[1];
if (args[0] !== "--golden" && (!tsc || (args[0] === "--raw" && !args[1]))) {
  console.error("usage: node regenerate.mjs [--raw OUT] path/to/tsc [path/to/tsc7]");
  process.exit(2);
}
const only = process.env.FIXTURES?.split(",");

const MAX_TYPE_ID = 3000;

const fixtures = {
  pnpm: {
    args: ["-b", ".", "--force"],
    prefix: "/home/ci/pnpm-repo",
    links: {
      "packages/ui/node_modules/csstype": "node_modules/.pnpm/csstype@3.1.3/node_modules/csstype",
      "packages/ui/node_modules/theme-kit": "node_modules/.pnpm/theme-kit@1.0.0/node_modules/theme-kit",
      "packages/app/node_modules/@acme/ui": "packages/ui",
      "packages/app/node_modules/theme-kit": "node_modules/.pnpm/theme-kit@1.0.0/node_modules/theme-kit",
      "node_modules/.pnpm/theme-kit@1.0.0/node_modules/csstype": "node_modules/.pnpm/csstype@3.1.2/node_modules/csstype",
    },
    keep: ["packages/ui/dist", "packages/app/dist"],
  },
  npm: { args: ["-p", "packages/web"], prefix: "/home/runner/work/npm-repo", links: {} },
  // Its hot spots compare no types, so it keeps no types.json.
  bun: { args: ["-p", "packages/server"], prefix: "/Users/dev/bun-repo", links: {}, types: false },
  deno: {
    args: ["-p", "packages/api"],
    prefix: "c:/users/ci/deno-repo",
    windows: true,
    links: {
      "node_modules/zod": "node_modules/.deno/zod@3.23.8/node_modules/zod",
      "node_modules/zod-form": "node_modules/.deno/zod-form@1.0.0/node_modules/zod-form",
      "node_modules/.deno/zod-form@1.0.0/node_modules/zod": "node_modules/.deno/zod@3.22.4/node_modules/zod",
    },
  },
  tsgo: {
    args: ["-p", "packages/web"],
    prefix: "/Users/Dev/tsgo-repo",
    lowercase: true,
    typescript: 7,
    links: {},
  },
};

if (args[0] === "--golden") {
  golden(args[1]);
  process.exit(0);
}

for (const [name, fixture] of Object.entries(fixtures)) {
  if (only && !only.includes(name)) continue;
  const compiler = fixture.typescript === 7 ? tsc7 : tsc;
  if (!compiler) {
    console.log(`${name}: left as it is; pass a TypeScript 7 tsc to regenerate it`);
    continue;
  }
  const src = path.join(here, name);
  const tmp = raw
    ? path.join(raw, name)
    : fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `oxiclean-trace-${name}-`)));
  const repo = path.join(tmp, "repo");
  if (raw) {
    fs.rmSync(tmp, { recursive: true, force: true });
    fs.mkdirSync(tmp, { recursive: true });
  }
  fs.cpSync(src, repo, { recursive: true, filter: (p) => !p.startsWith(path.join(src, "trace")) });
  for (const [link, target] of Object.entries(fixture.links)) {
    const at = path.join(repo, link);
    fs.mkdirSync(path.dirname(at), { recursive: true });
    fs.symlinkSync(path.relative(path.dirname(at), path.join(repo, target)), at, "dir");
  }
  const out = raw ? path.join(fs.realpathSync(repo), "trace") : path.join(tmp, "trace");
  try {
    execFileSync(compiler, [...fixture.args, "--generateTrace", out], { cwd: repo, stdio: "inherit" });
  } catch {
    // Type errors still write the trace.
  }
  if (raw) {
    console.log(`${name}: ${out}`);
    continue;
  }
  const dest = path.join(src, "trace");
  fs.rmSync(dest, { recursive: true, force: true });
  fs.mkdirSync(dest);
  // TypeScript 7 lowercases most paths in its types files, even where
  // file names are case-sensitive.
  const repoPattern = new RegExp(escapeRegExp(repo), "gi");
  const rewrite = (text, key) => {
    let s = text.replace(repoPattern, fixture.prefix);
    // TypeScript 7 keeps its lib files in a package of its platform's.
    s = s.replace(/"[^"]*\/node_modules\/(?:typescript|@typescript\/[^/"]+)\/lib\//g, '"/typescript/lib/');
    if (fixture.windows || fixture.lowercase) {
      // tsc lowercases `path`s on case-insensitive systems, but not file names.
      s = s.replace(/"(path|tracePath|typesPath|configFilePath)":"([^"]*)"/g, (_, k, v) => `"${k}":"${v.toLowerCase()}"`);
    }
    return s;
  };
  // Writes the types the trace names, by id, and `null` for the others.
  const keepTypes = (typesFile, ids) => {
    const types = JSON.parse(fs.readFileSync(path.join(out, typesFile), "utf8"));
    // Types past this id are left out, to keep the file small; the
    // trace then names types types.json doesn't have, as a trace whose
    // types.json was cut short does.
    const max = Math.min(MAX_TYPE_ID, Math.max(0, ...ids));
    const kept = types.slice(0, max).map((t, i) => (ids.has(i + 1) ? JSON.stringify(t) : "null"));
    fs.writeFileSync(path.join(dest, typesFile), rewrite("[" + kept.join(",\n") + "]\n"));
  };
  // Each trace's type ids, by the checker that named them.
  const checkerIds = new Map();
  for (const file of fs.readdirSync(out)) {
    if (!file.startsWith("trace")) continue;
    // Parse events of the fixture's own files name the packages loaded;
    // those of TypeScript's lib files, and bind events, say nothing.
    const events = JSON.parse(fs.readFileSync(path.join(out, file), "utf8")).filter(
      (e) =>
        e.cat !== "bind" &&
        e.cat !== "disabled-by-default-devtools.timeline" &&
        !(e.cat === "parse" && /\/(?:typescript|@typescript\/[^/]+)\/lib\//.test(e.args?.path ?? "")),
    );
    const ids = new Set();
    const byChecker = new Map();
    checkerIds.set(file, byChecker);
    for (const e of events) {
      for (const k of ["sourceId", "targetId", "id"]) {
        if (typeof e.args?.[k] === "number") {
          ids.add(e.args[k]);
          const checker = e.args.checkerId;
          if (!byChecker.has(checker)) byChecker.set(checker, new Set());
          byChecker.get(checker).add(e.args[k]);
        }
      }
    }
    fs.writeFileSync(path.join(dest, file), rewrite("[\n" + events.map((e) => JSON.stringify(e)).join(",\n") + "\n]\n"));
    const typesFile = "types" + file.slice("trace".length);
    if (fixture.types !== false && fs.existsSync(path.join(out, typesFile))) {
      keepTypes(typesFile, ids);
    }
  }
  if (fs.existsSync(path.join(out, "legend.json"))) {
    const legend = JSON.parse(fs.readFileSync(path.join(out, "legend.json"), "utf8"));
    // TypeScript 7's types files, one per checker, named in the legend.
    for (const p of legend) {
      const typesFile = p.typesPath && path.basename(p.typesPath);
      if (!typesFile || fixture.types === false || fs.existsSync(path.join(dest, typesFile))) continue;
      if (!fs.existsSync(path.join(out, typesFile))) continue;
      const ids = checkerIds.get(path.basename(p.tracePath))?.get(p.checkerId) ?? new Set();
      keepTypes(typesFile, ids);
    }
    const moved = legend.map((p) => ({
      ...p,
      tracePath: path.join(fixture.prefix, "trace", path.basename(p.tracePath)),
      typesPath: p.typesPath && path.join(fixture.prefix, "trace", path.basename(p.typesPath)),
    }));
    fs.writeFileSync(path.join(dest, "legend.json"), rewrite(JSON.stringify(moved, null, 2) + "\n"));
  }
  for (const dir of fixture.keep ?? []) {
    fs.rmSync(path.join(src, dir), { recursive: true, force: true });
    fs.cpSync(path.join(repo, dir), path.join(src, dir), {
      recursive: true,
      filter: (p) => !p.endsWith(".tsbuildinfo"),
    });
  }
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log(`${name}: ${fs.readdirSync(dest).join(", ")}`);
}

/** Writes each fixture's analyze-trace.json with the analyze-trace at `analyzeTrace`. */
function golden(analyzeTrace) {
  if (!analyzeTrace) {
    console.error("usage: node regenerate.mjs --golden path/to/analyze-trace");
    process.exit(2);
  }
  for (const [name, fixture] of Object.entries(fixtures)) {
    if (only && !only.includes(name)) continue;
    const root = fs.realpathSync(path.join(here, name));
    const tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `oxiclean-golden-${name}-`)));
    const trace = path.join(tmp, "trace");
    fs.mkdirSync(trace);
    for (const file of fs.readdirSync(path.join(root, "trace"))) {
      const text = fs.readFileSync(path.join(root, "trace", file), "utf8");
      // tsc lowercases paths on Windows and macOS, which analyze-trace
      // reads there in any case.
      const prefix = new RegExp(escapeRegExp(fixture.prefix), "gi");
      let moved = text.replace(prefix, root);
      // analyze-trace's stream parser drops the `null`s that stand for
      // the types left out, which moves every type after them to the
      // wrong id; empty records keep them in place.
      if (file.startsWith("types")) moved = moved.replace(/^(\[?)null(,|\])?$/gm, '$1{"flags":[]}$2');
      fs.writeFileSync(path.join(trace, file), moved);
    }
    const rc = JSON.parse(fs.readFileSync(path.join(root, ".oxicleanrc.json"), "utf8"));
    const [, { threshold, skipMillis }] = rc.rules["hot-spots"];
    let out;
    try {
      out = execFileSync(
        analyzeTrace,
        [trace, "--json", "--forceMillis", String(threshold), "--skipMillis", String(skipMillis)],
        { encoding: "utf8" },
      );
    } catch (e) {
      // analyze-trace exits 1 when it finds nothing.
      if (e.status !== 1) throw e;
      out = e.stdout;
    }
    const json = JSON.parse(out.split(trace).join("<trace>").split(root).join("<root>"));
    fs.writeFileSync(path.join(root, "analyze-trace.json"), JSON.stringify(json, null, 2) + "\n");
    fs.rmSync(tmp, { recursive: true, force: true });
    console.log(`${name}: analyze-trace.json`);
  }
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
