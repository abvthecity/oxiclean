# Benchmarks

`scripts/bench.sh` runs every checker against real-world
monorepos pinned in [`targets.tsv`](targets.tsv) and compares the results with
[`baseline.json`](baseline.json). Use it before and after a change to spot
regressions in runtime, findings, or import resolution.

```bash
scripts/bench.sh                      # compare against the baseline
scripts/bench.sh --update-baseline    # record a new baseline
scripts/bench.sh --install            # pnpm install each target first (needs network)
```

Repos are cached in `target/bench-repos` (override with `OXICLEAN_BENCH_DIR`).
Each check runs 3 times and the fastest time is kept (`OXICLEAN_BENCH_RUNS`).

## Current baseline

Recorded without installing dependencies (`installed: false`) on a 4-core
Linux machine. Imports between workspace packages resolve to their sources;
third-party packages such as `react` do not, which accounts for most of the
unresolved imports.

| Target | Check | Time | Files | Violations | Imports resolved |
|---|---|---|---|---|---|
| Next.js | import-bloat | ~1.0s | 2,340 | 366 | 88.7% |
| Next.js | import-depth | ~1.0s | 2,340 | ~610 | 88.7% |
| Expo | import-bloat | ~0.8s | 4,656 | 1,001 | 74.7% |
| Expo | import-depth | ~0.6s | 4,656 | ~3,928 | 74.7% |

Known issue: `import-depth` results vary slightly between runs because depths
computed inside import cycles depend on traversal order, so its violation
count is approximate until that is fixed.
