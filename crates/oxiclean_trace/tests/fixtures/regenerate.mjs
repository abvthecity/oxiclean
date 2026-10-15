// Regenerates the traces of the fixtures in this directory with a real tsc:
//
//   node crates/oxiclean_trace/tests/fixtures/regenerate.mjs path/to/tsc
//   node crates/oxiclean_trace/tests/fixtures/regenerate.mjs --raw OUT path/to/tsc
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
// lowercases paths. The pnpm fixture is traced with `tsc --build`, so it
// has the legend.json layout, and keeps the declarations it emits.
//
// Timings differ from run to run, so the expected output in tests/fixtures.rs
// changes with every regeneration.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const here = path.dirname(new URL(import.meta.url).pathname);
const args = process.argv.slice(2);
const raw = args[0] === "--raw" ? path.resolve(args[1]) : undefined;
const tsc = raw ? args[2] : args[0];
if (!tsc || (args[0] === "--raw" && !args[1])) {
  console.error("usage: node regenerate.mjs [--raw OUT] path/to/tsc");
  process.exit(2);
}

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
  bun: { args: ["-p", "packages/server"], prefix: "/Users/dev/bun-repo", links: {} },
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
};

for (const [name, fixture] of Object.entries(fixtures)) {
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
    execFileSync(tsc, [...fixture.args, "--generateTrace", out], { cwd: repo, stdio: "inherit" });
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
  const rewrite = (text, key) => {
    let s = text.split(repo).join(fixture.prefix);
    s = s.replace(/"[^"]*\/node_modules\/typescript\/lib\//g, '"/typescript/lib/');
    if (fixture.windows) {
      // tsc lowercases `path`s on case-insensitive systems, but not file names.
      s = s.replace(/"(path|tracePath|typesPath|configFilePath)":"([^"]*)"/g, (_, k, v) => `"${k}":"${v.toLowerCase()}"`);
    }
    return s;
  };
  for (const file of fs.readdirSync(out)) {
    if (!file.startsWith("trace")) continue;
    // Parse events of the fixture's own files name the packages loaded;
    // those of TypeScript's lib files, and bind events, say nothing.
    const events = JSON.parse(fs.readFileSync(path.join(out, file), "utf8")).filter(
      (e) =>
        e.cat !== "bind" &&
        e.cat !== "disabled-by-default-devtools.timeline" &&
        !(e.cat === "parse" && /\/typescript\/lib\//.test(e.args?.path ?? "")),
    );
    const ids = new Set();
    for (const e of events) {
      for (const k of ["sourceId", "targetId", "id"]) {
        if (typeof e.args?.[k] === "number") ids.add(e.args[k]);
      }
    }
    fs.writeFileSync(path.join(dest, file), rewrite("[\n" + events.map((e) => JSON.stringify(e)).join(",\n") + "\n]\n"));
    const typesFile = "types" + file.slice("trace".length);
    if (fs.existsSync(path.join(out, typesFile))) {
      const types = JSON.parse(fs.readFileSync(path.join(out, typesFile), "utf8"));
      // Types past this id are left out, to keep the file small; the
      // trace then names types types.json doesn't have, as a trace whose
      // types.json was cut short does.
      const max = Math.min(MAX_TYPE_ID, Math.max(0, ...ids));
      const kept = types.slice(0, max).map((t, i) => (ids.has(i + 1) ? JSON.stringify(t) : "null"));
      fs.writeFileSync(path.join(dest, typesFile), rewrite("[" + kept.join(",\n") + "]\n"));
    }
  }
  if (fs.existsSync(path.join(out, "legend.json"))) {
    const legend = JSON.parse(fs.readFileSync(path.join(out, "legend.json"), "utf8"));
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
