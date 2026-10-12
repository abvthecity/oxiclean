#!/usr/bin/env bash
# Run oxiclean and knip side by side on the monorepos pinned in bench/targets.tsv,
# for the checks both tools have, and compare speed and findings.
#
# Usage: scripts/bench-knip.sh [check...]
#   check   oxiclean check to compare (default: all with a knip equivalent).
#           Supported: import-cycles (knip `cycles`).
#
# Both tools get the same entry files: oxiclean's default (every non-test JS/TS
# file inside a `src` directory). knip runs with that as `entry` and `project`
# in every workspace and with all of its plugins off, so neither tool picks up
# entries the other doesn't know about. Repos are cached as in scripts/bench.sh
# ($OXICLEAN_BENCH_DIR) and are not installed; knip is installed once into
# $OXICLEAN_BENCH_DIR/.knip. Each tool runs $OXICLEAN_BENCH_RUNS times and the
# fastest time is kept. Detailed results go to $OXICLEAN_BENCH_DIR/.knip/results.
# Set OXICLEAN_BENCH_TARGETS to a space-separated list of names to run only those.
#
# The cycle counts measure different things: knip reports one cycle per back
# edge of its depth-first search, oxiclean picks short cycles until removing
# the last import of each leaves none. `knip_missed` counts knip cycles that
# none of oxiclean's last imports breaks, and should be 0. `ox_only` counts
# oxiclean cycles that share no file with any knip cycle.
#
# Needs bash 4+, GNU date, jq and Node.js (Linux).
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT=$(pwd)
BENCH_DIR=${OXICLEAN_BENCH_DIR:-$REPO_ROOT/target/bench-repos}
RUNS=${OXICLEAN_BENCH_RUNS:-3}
TARGETS=${OXICLEAN_BENCH_TARGETS:-}
KNIP_VERSION=6.41.0
KNIP_DIR=$BENCH_DIR/.knip
OUT_DIR=$KNIP_DIR/results

declare -A KNIP_TYPE=([import-cycles]=cycles)
checks=("$@")
if [[ ${#checks[@]} -eq 0 ]]; then checks=("${!KNIP_TYPE[@]}"); fi
for check in "${checks[@]}"; do
  [[ -n ${KNIP_TYPE[$check]:-} ]] || { echo "no knip equivalent for: $check" >&2; exit 2; }
done

cargo build --release -p oxiclean >&2
BIN=$REPO_ROOT/target/release/oxiclean
mkdir -p "$OUT_DIR"
if [[ ! -x $KNIP_DIR/node_modules/.bin/knip ]]; then
  echo "Installing knip@$KNIP_VERSION..." >&2
  npm install --silent --prefix "$KNIP_DIR" "knip@$KNIP_VERSION" typescript >&2
fi
KNIP=$KNIP_DIR/node_modules/.bin/knip

# A knip config with oxiclean's default entries in every workspace and every
# plugin turned off.
CONFIG=$KNIP_DIR/knip.json
(cd "$KNIP_DIR" && node --input-type=module -e '
  const { Plugins } = await import("./node_modules/knip/dist/plugins/index.js");
  const files = "**/src/**/*.{js,jsx,ts,tsx,mjs,cjs,mts,cts}";
  const skip = ["**/*.{test,spec}.*", "**/__tests__/**", "**/__mocks__/**", "**/__fixtures__/**", "**/*.d.{ts,mts,cts}"];
  const workspace = { entry: [files, ...skip.map(p => "!" + p)], project: [files] };
  for (const name of Object.keys(Plugins)) workspace[name] = false;
  process.stdout.write(JSON.stringify({ workspaces: { ".": workspace, "**": workspace } }, null, 2));
') >"$CONFIG"

ms_now() { date +%s%3N; }

# Usage: best_time DIR OUT CMD...
# Runs CMD in DIR $RUNS times and prints the fastest time in ms, or FAIL when
# a run exits with anything but 0 or 1 (findings) or writes invalid JSON. The
# output of the last run is written to OUT, and its stderr to OUT.err.
best_time() {
  local dir=$1 out=$2 best= start elapsed status
  shift 2
  for _ in $(seq "$RUNS"); do
    start=$(ms_now)
    status=0
    (cd "$dir" && "$@") >"$out" 2>"$out.err" || status=$?
    elapsed=$(($(ms_now) - start))
    if [[ $status -gt 1 ]] || ! jq -e . "$out" >/dev/null 2>&1; then
      echo "FAIL"
      return
    fi
    if [[ -z $best || $elapsed -lt $best ]]; then best=$elapsed; fi
  done
  echo "$best"
}

# Prints: our cycles, knip's cycles, knip cycles that none of our closing
# imports breaks (should be 0), and our cycles with no file in any knip cycle.
compare_cycles() {
  local ours=$1 theirs=$2
  jq -rn --slurpfile o "$ours" --slurpfile k "$theirs" '
    def edges: [range(length) as $i | [.[$i], .[($i + 1) % length]]];
    ($o[0].findings | map([.cycle[].file])) as $oc
    | ($k[0].issues | map(.cycles // [] | map(map(.name))) | add // []) as $kc
    | ($oc | map([.[-1], .[0]])) as $closing
    | ($kc | add // [] | unique) as $kf
    | ($kc | map(select(edges | map(. as $e | $closing | index([$e]) != null) | any | not))
       | length) as $missed
    | ($oc | map(select(map(. as $f | $kf | index($f) != null) | any | not)) | length) as $only
    | "\($oc | length) \($kc | length) \($missed) \($only)"'
}

printf '\n%-10s %-13s %10s %10s %7s %7s %12s %9s\n' \
  target check oxiclean knip ox_cyc knip_cyc knip_missed ox_only
while IFS=$'\t' read -r name _ sha; do
  [[ -z $name || $name == \#* ]] && continue
  if [[ -n $TARGETS && " $TARGETS " != *" $name "* ]]; then continue; fi
  dir=$BENCH_DIR/$name
  if [[ $(git -C "$dir" rev-parse HEAD 2>/dev/null || true) != "$sha" ]]; then
    echo "Skipping $name: run scripts/bench.sh first to check out ${sha:0:12}" >&2
    continue
  fi
  for check in "${checks[@]}"; do
    type=${KNIP_TYPE[$check]}
    echo "Running $check and knip --include $type on $name..." >&2
    ours=$OUT_DIR/$name.$check.json
    theirs=$OUT_DIR/$name.knip-$type.json
    # No .oxicleanrc.json in the targets, so oxiclean runs with its defaults.
    ox_ms=$(best_time "$dir" "$ours" env NO_COLOR=1 "$BIN" "$check" --root . --format json)
    knip_ms=$(best_time "$dir" "$theirs" "$KNIP" --config "$CONFIG" --include "$type" \
      --reporter json --no-progress --no-exit-code --no-config-hints)
    if [[ $ox_ms == FAIL || $knip_ms == FAIL ]]; then
      printf '%-10s %-13s %10s %10s   failed; see %s.err\n' "$name" "$check" "$ox_ms" "$knip_ms" \
        "$([[ $ox_ms == FAIL ]] && echo "$ours" || echo "$theirs")"
      continue
    fi
    read -r oc kc missed only < <(compare_cycles "$ours" "$theirs")
    printf '%-10s %-13s %8sms %8sms %7s %7s %12s %9s\n' \
      "$name" "$check" "$ox_ms" "$knip_ms" "$oc" "$kc" "$missed" "$only"
  done
done < bench/targets.tsv
