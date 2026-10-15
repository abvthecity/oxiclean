#!/usr/bin/env bash
# Run oxiclean and knip side by side on the monorepos pinned in bench/targets.tsv,
# for the checks both tools have, and compare speed and findings.
#
# Usage: scripts/bench-knip.sh [check...]
#   check   oxiclean rule to compare (default: all with a knip equivalent). Each
#           runs as its command with a config file that turns the command's
#           other rules off.
#           Supported: cycles and duplicates, which knip names the same,
#           unused-exports and unused-types, which knip calls exports and types,
#           and unused-enum-members and unused-namespace-members, which knip
#           calls enumMembers and namespaceMembers.
#
# Both tools get the same entry files, with all of knip's plugins off so
# neither tool picks up entries the other doesn't know about:
# - For cycles and duplicates, oxiclean's default (every non-test JS/TS file
#   inside a `src` directory). knip runs with that as `entry` and `project` in
#   every workspace, so it only reports on files inside `src` directories,
#   while oxiclean also reports on files outside them that the entries import.
#   (Making every file a knip project file instead makes knip analyse files no
#   entry reaches.)
# - For unused exports, types and members, the default entries of both tools: each
#   package's package.json `main`, `bin` and `exports` and its `index`, `main`
#   and `cli` files, plus test files, which knip's test-runner plugins would
#   add.
#
# Repos are cached as in scripts/bench.sh
# ($OXICLEAN_BENCH_DIR) and are not installed; knip is installed once into
# $OXICLEAN_BENCH_DIR/.knip. Each tool runs $OXICLEAN_BENCH_RUNS times and the
# fastest time is kept. Detailed results go to $OXICLEAN_BENCH_DIR/.knip/results.
# Set OXICLEAN_BENCH_TARGETS to a space-separated list of names to run only those.
#
# `ox` and `knip` count each tool's findings. For cycles they measure
# different things: knip reports one cycle per back edge of its depth-first
# search, oxiclean picks short cycles until removing the last import of each
# leaves none. `knip_missed` counts knip cycles that none of oxiclean's last
# imports breaks, and should be 0; `ox_only` counts oxiclean cycles that share
# no file with any knip cycle. For duplicate exports, a finding matches when
# both tools list the same names in the same file: `knip_missed` counts knip
# findings oxiclean lacks and `ox_only` the reverse. For unused exports,
# types and members, a finding is a file and a name (`Enum.member` for a
# member), compared the same way.
#
# Needs bash 4+, GNU date, jq and Node.js (Linux).
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT=$(pwd)
# shellcheck source=scripts/oxiclean-rules.sh
source scripts/oxiclean-rules.sh
BENCH_DIR=${OXICLEAN_BENCH_DIR:-$REPO_ROOT/target/bench-repos}
RUNS=${OXICLEAN_BENCH_RUNS:-3}
TARGETS=${OXICLEAN_BENCH_TARGETS:-}
KNIP_VERSION=6.41.0
KNIP_DIR=$BENCH_DIR/.knip
OUT_DIR=$KNIP_DIR/results

declare -A KNIP_TYPE=([cycles]=cycles [duplicates]=duplicates [unused-exports]=exports
  [unused-types]=types [unused-enum-members]=enumMembers
  [unused-namespace-members]=namespaceMembers)
checks=("$@")
if [[ ${#checks[@]} -eq 0 ]]; then checks=("${!KNIP_TYPE[@]}"); fi
for check in "${checks[@]}"; do
  [[ -n ${KNIP_TYPE[$check]:-} ]] || { echo "no knip equivalent for: $check" >&2; exit 2; }
done

cargo build --release -p oxiclean >&2
BIN=$REPO_ROOT/target/release/oxiclean
mkdir -p "$OUT_DIR"
# Each check is one rule, run by its command with a config file that turns
# the command's other rules off.
OX_CONFIG_DIR=$(mktemp -d)
trap 'rm -rf "$OX_CONFIG_DIR"' EXIT
for check in "${checks[@]}"; do only_rule_config "$check" "$OX_CONFIG_DIR/$check.json"; done
if [[ ! -x $KNIP_DIR/node_modules/.bin/knip ]]; then
  echo "Installing knip@$KNIP_VERSION..." >&2
  npm install --silent --prefix "$KNIP_DIR" "knip@$KNIP_VERSION" typescript >&2
fi
KNIP=$KNIP_DIR/node_modules/.bin/knip

# knip configs with every plugin turned off. knip.json has oxiclean's default
# entries in every workspace; knip-exports.json has knip's own default entries
# plus test files.
CONFIG=$KNIP_DIR/knip.json
EXPORTS_CONFIG=$KNIP_DIR/knip-exports.json
knip_config() {
  (cd "$KNIP_DIR" && node --input-type=module -e '
    const { Plugins } = await import("./node_modules/knip/dist/plugins/index.js");
    const ext = "{js,jsx,ts,tsx,mjs,cjs,mts,cts}";
    let workspace;
    if (process.argv[1] === "exports") {
      const entry = [`{index,cli,main}.${ext}`, `src/{index,cli,main}.${ext}`,
        `**/*.{test,spec}.${ext}`, `**/__tests__/**/*.${ext}`];
      workspace = { entry, project: [`**/*.${ext}`] };
    } else {
      const files = `**/src/**/*.${ext}`;
      const skip = ["**/*.{test,spec}.*", "**/__tests__/**", "**/__mocks__/**", "**/__fixtures__/**", "**/*.d.{ts,mts,cts}"];
      workspace = { entry: [files, ...skip.map(p => "!" + p)], project: [files] };
    }
    for (const name of Object.keys(Plugins)) workspace[name] = false;
    process.stdout.write(JSON.stringify({ workspaces: { ".": workspace, "**": workspace } }, null, 2));
  ' "$1")
}
knip_config src >"$CONFIG"
knip_config exports >"$EXPORTS_CONFIG"

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
    ($o[0].findings | map(select(.rule == "cycles") | [.cycle[].file])) as $oc
    | ($k[0].issues | map(.cycles // [] | map(map(.name))) | add // []) as $kc
    | ($oc | map([.[-1], .[0]])) as $closing
    | ($kc | add // [] | unique) as $kf
    | ($kc | map(select(edges | map(. as $e | $closing | index([$e]) != null) | any | not))
       | length) as $missed
    | ($oc | map(select(map(. as $f | $kf | index($f) != null) | any | not)) | length) as $only
    | "\($oc | length) \($kc | length) \($missed) \($only)"'
}

# Prints: our duplicates, knip's, knip's that we lack, and ours that knip
# lacks. A duplicate is a file and its set of names.
compare_duplicates() {
  local ours=$1 theirs=$2
  jq -rn --slurpfile o "$ours" --slurpfile k "$theirs" '
    ($o[0].findings | map(select(.rule == "duplicates") | {file, names: ([.names[].name] | sort)})) as $od
    | ($k[0].issues | map(.file as $f | (.duplicates // [])[] | {file: $f, names: (map(.name) | sort)})) as $kd
    | "\($od | length) \($kd | length) \($kd - $od | length) \($od - $kd | length)"'
}

# Prints: our unused exports (or types, or members), knip's, knip's that we
# lack, and ours that knip lacks. A finding is a file and a name; knip names a
# member by its export in `namespace`.
compare_exports() {
  local ours=$1 theirs=$2 type=${3:-exports} rule=${4:-unused-exports}
  jq -rn --slurpfile o "$ours" --slurpfile k "$theirs" --arg type "$type" --arg rule "$rule" '
    ($o[0].findings | map(select(.rule == $rule) | {file, name}) | unique) as $ou
    | ($k[0].issues | map(.file as $f | (.[$type] // [])[]
        | {file: $f, name: (if .namespace then "\(.namespace).\(.name)" else .name end)})
      | unique) as $ku
    | "\($ou | length) \($ku | length) \($ku - $ou | length) \($ou - $ku | length)"'
}

compare_types() { compare_exports "$1" "$2" types unused-types; }
compare_enumMembers() { compare_exports "$1" "$2" enumMembers unused-enum-members; }
compare_namespaceMembers() {
  compare_exports "$1" "$2" namespaceMembers unused-namespace-members
}

printf '\n%-10s %-25s %10s %10s %7s %7s %12s %9s\n' \
  target check oxiclean knip ox knip knip_missed ox_only
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
    # No .oxicleanrc.json in the targets, so oxiclean runs with its defaults
    # but for the command's other rules, which the config turns off.
    ox_ms=$(best_time "$dir" "$ours" env NO_COLOR=1 "$BIN" "$(command_of "$check")" --root . \
      -c "$OX_CONFIG_DIR/$check.json" --format json)
    # A rule that couldn't run is a failed run, though the command exits 1
    # as for findings.
    if [[ $ox_ms != FAIL ]] && ! jq -e --arg rule "$check" \
      '[.rules[] | select(.rule == $rule and .error == null)] | length == 1' "$ours" >/dev/null; then
      jq -r '.rules[] | select(.error) | "\(.rule) could not run: \(.error)"' "$ours" >>"$ours.err"
      ox_ms=FAIL
    fi
    config=$CONFIG
    if [[ $type != cycles && $type != duplicates ]]; then config=$EXPORTS_CONFIG; fi
    knip_ms=$(best_time "$dir" "$theirs" "$KNIP" --config "$config" --include "$type" \
      --reporter json --no-progress --no-exit-code --no-config-hints)
    if [[ $ox_ms == FAIL || $knip_ms == FAIL ]]; then
      printf '%-10s %-25s %10s %10s   failed; see %s.err\n' "$name" "$check" "$ox_ms" "$knip_ms" \
        "$([[ $ox_ms == FAIL ]] && echo "$ours" || echo "$theirs")"
      continue
    fi
    read -r oc kc missed only < <("compare_${type}" "$ours" "$theirs")
    printf '%-10s %-25s %8sms %8sms %7s %7s %12s %9s\n' \
      "$name" "$check" "$ox_ms" "$knip_ms" "$oc" "$kc" "$missed" "$only"
  done
done < bench/targets.tsv
