#!/usr/bin/env bash
# Compares `oxiclean trace` with @typescript/analyze-trace on fresh traces:
# one of each trace fixture (pnpm, npm, deno and bun layouts), and, when
# given, one of a real repository. Both tools read the same trace with the
# same thresholds; scripts/analyze-trace-parity.mjs compares their JSON.
#
#   scripts/analyze-trace-parity.sh [REPO [TSC_ARGS...]]
#
# REPO is a checkout with its dependencies installed, traced with
# `tsc TSC_ARGS --generateTrace` from its root (TSC_ARGS defaults to `-b`).
#
# Environment:
#   TSC            tsc to trace with (default: tsc on PATH)
#   ANALYZE_TRACE  analyze-trace command (default: npx analyze-trace 0.11.1)
#   OXICLEAN       oxiclean binary (default: target/release/oxiclean, built
#                  when missing)
#   FORCE_MILLIS   analyze-trace --forceMillis and the hot-spots threshold
#                  (default: 5 for the fixtures, 100 for REPO)
#   SKIP_MILLIS    analyze-trace --skipMillis and skipMillis (default: 1
#                  for the fixtures, 20 for REPO)
#   TOLERANCE_MS   how far times may differ (default: 1)
#   WORK           where traces and outputs go (default: a new temp dir)
#
# Exits 1 when the tools disagree on any trace.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tsc="${TSC:-tsc}"
oxiclean="${OXICLEAN:-$here/target/release/oxiclean}"
work="${WORK:-$(mktemp -d)}"
tolerance="${TOLERANCE_MS:-1}"
read -r -a analyze_trace <<< "${ANALYZE_TRACE:-npx --yes --package @typescript/analyze-trace@0.11.1 analyze-trace}"

if [[ ! -x "$oxiclean" ]]; then
  cargo build --release --manifest-path "$here/Cargo.toml" -p oxiclean
fi
mkdir -p "$work"
echo "tsc $("$tsc" --version), output in $work"

failed=0

# compare NAME ROOT TRACE_DIR FORCE SKIP
compare() {
  local name="$1" root="$2" trace="$3" force="$4" skip="$5"
  local out="$work/$name"
  mkdir -p "$out"
  # Only the hot-spot settings; complex-types would only raise severities.
  printf '{ "rules": { "complex-types": "off", "hot-spots": ["warn", { "threshold": %s, "skipMillis": %s }] } }\n' \
    "$force" "$skip" > "$out/oxicleanrc.json"
  # analyze-trace exits 1 when it finds nothing, and 2 when it fails.
  "${analyze_trace[@]}" "$trace" --json --forceMillis "$force" --skipMillis "$skip" > "$out/analyze-trace.json" || [[ $? -eq 1 ]]
  # oxiclean exits 1 when a finding is an error.
  "$oxiclean" trace "$trace" --root "$root" -c "$out/oxicleanrc.json" --format json > "$out/oxiclean.json" || [[ $? -eq 1 ]]
  echo "== $name (forceMillis $force, skipMillis $skip)"
  if ! node "$here/scripts/analyze-trace-parity.mjs" "$out/analyze-trace.json" "$out/oxiclean.json" "$root" "$tolerance"; then
    failed=1
  fi
}

node "$here/crates/oxiclean_trace/tests/fixtures/regenerate.mjs" --raw "$work/fixtures" "$tsc"
for name in pnpm npm deno bun; do
  compare "$name" "$work/fixtures/$name/repo" "$work/fixtures/$name/repo/trace" "${FORCE_MILLIS:-5}" "${SKIP_MILLIS:-1}"
done

# tsc only records findSourceFile events that take 10ms or more, so fresh
# traces of small repos seldom show analyze-trace a duplicate package. This
# one is written by hand: npm-nested and pnpm copies, each slow to load.
dup="$work/fixtures/duplicates"
rm -rf "$dup"
node - "$dup" <<'EOF'
const fs = require("fs");
const path = require("path");
const root = process.argv[2];
const dirs = [
  ["node_modules/aa", "1.0.0"],
  ["node_modules/bb/node_modules/aa", "2.0.0"],
  ["node_modules/@s/cc", "1.0.0"],
  ["node_modules/.pnpm/@s+cc@1.1.0/node_modules/@s/cc", "1.1.0"],
];
const events = dirs.map(([dir, version], i) => {
  fs.mkdirSync(path.join(root, dir), { recursive: true });
  fs.writeFileSync(path.join(root, dir, "package.json"), JSON.stringify({ name: dir.split("node_modules/").pop(), version }));
  fs.writeFileSync(path.join(root, dir, "index.d.ts"), "export {};\n");
  const fileName = path.join(fs.realpathSync(root), dir, "index.d.ts");
  return { pid: 1, tid: 1, ph: "X", cat: "program", ts: 1000 + 30000 * i, dur: 20000, name: "findSourceFile", args: { fileName } };
});
fs.mkdirSync(path.join(root, "trace"));
fs.writeFileSync(path.join(root, "trace", "trace.json"), JSON.stringify(events));
EOF
compare duplicates "$dup" "$dup/trace" "${FORCE_MILLIS:-5}" "${SKIP_MILLIS:-1}"

if [[ $# -gt 0 ]]; then
  repo="$(cd "$1" && pwd)"
  shift
  args=("$@")
  [[ ${#args[@]} -gt 0 ]] || args=(-b)
  rm -rf "$work/repo-trace"
  # Type errors still write the trace.
  (cd "$repo" && "$tsc" "${args[@]}" --generateTrace "$work/repo-trace") || true
  compare "$(basename "$repo")" "$repo" "$work/repo-trace" "${FORCE_MILLIS:-100}" "${SKIP_MILLIS:-20}"
fi

exit "$failed"
