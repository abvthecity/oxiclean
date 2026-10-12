#!/usr/bin/env bash
# Run oxiclean's checkers against real-world monorepos pinned in bench/targets.tsv
# and compare against bench/baseline.json.
#
# Usage: scripts/bench.sh [--install] [--update-baseline]
#   --install          install each target's dependencies first (`pnpm install` or `npm ci`,
#                      without scripts), so third-party packages resolve too (slower, needs network)
#   --update-baseline  overwrite bench/baseline.json with this run's results
#
# Repos are cached in $OXICLEAN_BENCH_DIR (default: target/bench-repos).
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT=$(pwd)
BENCH_DIR=${OXICLEAN_BENCH_DIR:-$REPO_ROOT/target/bench-repos}
BASELINE=$REPO_ROOT/bench/baseline.json
RUNS=${OXICLEAN_BENCH_RUNS:-3}
CHECKS=(bloat depth cycles)

install=false
update=false
for arg in "$@"; do
  case $arg in
    --install) install=true ;;
    --update-baseline) update=true ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

cargo build --release -p oxiclean >&2
BIN=$REPO_ROOT/target/release/oxiclean
mkdir -p "$BENCH_DIR"

checkout() {
  local name=$1 url=$2 sha=$3 dir=$BENCH_DIR/$1
  if [[ ! -d $dir/.git ]]; then
    git init -q "$dir"
    git -C "$dir" remote add origin "$url"
  fi
  if [[ $(git -C "$dir" rev-parse HEAD 2>/dev/null || true) != "$sha" ]]; then
    echo "Fetching $name@${sha:0:12}..." >&2
    git -C "$dir" fetch -q --depth 1 origin "$sha"
    git -C "$dir" checkout -q --force "$sha"
  fi
  if $install; then
    echo "Installing dependencies for $name..." >&2
    if [[ -f $dir/package-lock.json ]]; then
      (cd "$dir" && npm ci --ignore-scripts >&2)
    else
      (cd "$dir" && pnpm install --frozen-lockfile --ignore-scripts >&2)
    fi
  fi
}

# Prints: elapsed_ms files violations resolved total builtins
run_check() {
  local check=$1 dir=$2 out best_ms=
  for _ in $(seq "$RUNS"); do
    # Checkers exit 1 when they find violations; that is expected here
    out=$(NO_COLOR=1 "$BIN" "$check" --root "$dir" 2>/dev/null || true)
    local ms
    ms=$(sed -nE 's/.*Finished in ([0-9]+)ms.*/\1/p' <<<"$out")
    if [[ -z $best_ms || $ms -lt $best_ms ]]; then best_ms=$ms; fi
  done
  local files violations resolved total builtins
  files=$(sed -nE 's/.*Finished in [0-9]+ms on ([0-9]+) files.*/\1/p' <<<"$out")
  violations=$(sed -nE 's/^ *Total violations: ([0-9]+).*/\1/p' <<<"$out")
  read -r resolved total builtins < <(sed -nE 's/.*Resolved ([0-9]+) of ([0-9]+) imports.*, ([0-9]+) built-ins.*/\1 \2 \3/p' <<<"$out")
  echo "$best_ms $files ${violations:-0} $resolved $total $builtins"
}

results="[]"
while IFS=$'\t' read -r name url sha; do
  [[ -z $name || $name == \#* ]] && continue
  checkout "$name" "$url" "$sha"
  for check in "${CHECKS[@]}"; do
    echo "Running $check on $name..." >&2
    read -r ms files violations resolved total builtins < <(run_check "$check" "$BENCH_DIR/$name")
    results=$(jq -c --arg name "$name" --arg sha "$sha" --arg check "$check" \
      --argjson ms "$ms" --argjson files "$files" --argjson violations "$violations" \
      --argjson resolved "$resolved" --argjson total "$total" --argjson builtins "$builtins" \
      --argjson installed "$install" \
      '. + [{target: $name, commit: $sha, check: $check, installed: $installed, elapsed_ms: $ms,
             files: $files, violations: $violations, imports_resolved: $resolved,
             imports_total: $total, builtins: $builtins}]' <<<"$results")
  done
done < bench/targets.tsv

# Report against the baseline
printf '\n%-10s %-13s %10s %8s %11s %10s\n' target check time_ms files violations resolved
jq -r --slurpfile base <(cat "$BASELINE" 2>/dev/null || echo '[]') '
  def pct(r; t): if t == 0 then "-" else ((r * 1000 / t | floor) / 10 | tostring) + "%" end;
  def delta(new; old): if old == null then "" else " (" + (if new >= old then "+" else "" end) + ((new - old) | tostring) + ")" end;
  .[] as $r
  | ($base[0] // [] | map(select(.target == $r.target and .check == $r.check)) | first) as $b
  | [$r.target, $r.check,
     ($r.elapsed_ms | tostring) + delta($r.elapsed_ms; $b.elapsed_ms),
     ($r.files | tostring) + delta($r.files; $b.files),
     ($r.violations | tostring) + delta($r.violations; $b.violations),
     pct($r.imports_resolved; $r.imports_total)]
  | @tsv' <<<"$results" | while IFS=$'\t' read -r t c ms f v p; do
    printf '%-10s %-13s %10s %8s %11s %10s\n' "$t" "$c" "$ms" "$f" "$v" "$p"
  done

if $update; then
  jq '.' <<<"$results" >"$BASELINE"
  echo "Updated $BASELINE" >&2
fi
