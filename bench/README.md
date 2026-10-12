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
| Next.js | import-bloat | ~0.8s | 2,344 | 164 | 88.6% |
| Next.js | import-depth | ~0.8s | 2,344 | 202 | 88.6% |
| Expo | import-bloat | ~0.5s | 4,656 | 868 | 74.7% |
| Expo | import-depth | ~0.5s | 4,656 | 1,586 | 74.7% |

Results are deterministic, so any change in violations comes from a code change.

## Choosing thresholds

The default thresholds (300 modules for `import-bloat`, 20 levels for
`import-depth`) come from the distribution of each file's worst value in
these targets, measured with `--threshold 1 --format json`:

| Target | Check | Files | p90 | p95 | p99 | Files flagged at default |
|---|---|---|---|---|---|---|
| Next.js | import-bloat | 2,129 | 172 | 309 | 966 | 112 (5.3%) |
| Expo | import-bloat | 4,319 | 217 | 497 | 585 | 400 (9.3%) |
| Next.js | import-depth | 1,294 | 18 | 23 | 29 | 104 (8.0%) |
| Expo | import-depth | 3,112 | 41 | 43 | 50 | 736 (23.7%) |

The old defaults (200 and 10) flagged 9–11% of files for bloat and 22–57% for
depth, which buried the outliers. Expo's long import chains are concentrated in
`apps/native-component-list` (394 files) and `packages/expo-router` (151), which
is what per-package `[[overrides]]` are for. These numbers are without
`node_modules`; with dependencies installed, third-party modules count too and
values go up.
