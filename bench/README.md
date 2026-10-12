# Benchmarks

`scripts/bench.sh` runs every checker against real-world
monorepos pinned in [`targets.tsv`](targets.tsv) and compares the results with
[`baseline.json`](baseline.json). Use it before and after a change to spot
regressions in runtime, findings, or import resolution.

```bash
scripts/bench.sh                      # compare against the baseline
scripts/bench.sh --update-baseline    # record a new baseline
scripts/bench.sh --install            # install each target first: pnpm install or npm ci (needs network)
scripts/bench-knip.sh                 # compare with knip on the checks both tools have
```

## Targets

Popular npm and pnpm monorepos with many dependencies. None of them uses
knip (no knip config and no `knip` in the root `package.json`).

| Target | Package manager | Repository |
|---|---|---|
| Next.js | pnpm | vercel/next.js |
| Expo | pnpm | expo/expo |
| MUI | pnpm | mui/material-ui |
| Vite | pnpm | vitejs/vite |
| Playwright | npm | microsoft/playwright |
| Lit | npm | lit/lit |

Repos are cached in `target/bench-repos` (override with `OXICLEAN_BENCH_DIR`).
Each check runs 3 times and the fastest time is kept (`OXICLEAN_BENCH_RUNS`).

## Current baseline

Recorded without installing dependencies (`installed: false`) on a 4-core
Linux machine. Imports between workspace packages resolve to their sources;
third-party packages such as `react` do not, which accounts for most of the
unresolved imports.

| Target | Check | Time | Files | Violations | Imports resolved |
|---|---|---|---|---|---|
| Next.js | bloat | ~1.0s | 2,344 | 164 | 88.6% |
| Next.js | depth | ~1.0s | 2,344 | 202 | 88.6% |
| Next.js | cycles | ~1.0s | 2,344 | 68 | 88.6% |
| Expo | bloat | ~0.7s | 4,656 | 868 | 74.7% |
| Expo | depth | ~0.7s | 4,656 | 1,586 | 74.7% |
| Expo | cycles | ~0.7s | 4,656 | 46 | 74.7% |
| MUI | bloat | ~0.5s | 13,301 | 690 | 68.3% |
| MUI | depth | ~0.5s | 13,301 | 3,788 | 68.3% |
| MUI | cycles | ~0.5s | 13,301 | 4 | 68.3% |
| Vite | bloat | ~40ms | 367 | 0 | 72.6% |
| Vite | depth | ~50ms | 367 | 0 | 72.6% |
| Vite | cycles | ~50ms | 367 | 49 | 72.6% |
| Playwright | bloat | ~50ms | 765 | 75 | 87.5% |
| Playwright | depth | ~55ms | 765 | 53 | 87.5% |
| Playwright | cycles | ~60ms | 765 | 76 | 87.5% |
| Lit | bloat | ~70ms | 754 | 0 | 49.7% |
| Lit | depth | ~70ms | 754 | 0 | 49.7% |
| Lit | cycles | ~60ms | 754 | 20 | 49.7% |

Results are deterministic, so any change in violations comes from a code change.
Times vary by about 20% between runs on shared machines; compare against a
baseline recorded on the same machine.

## Comparison with knip

`scripts/bench-knip.sh` runs oxiclean's release build and
[knip](https://knip.dev) 6.41.0 on the same targets, with the same entry
files: oxiclean's default (every non-test JS/TS file in a `src` directory),
given to knip as `entry` and `project` in every workspace, with all knip
plugins off. Neither tool installs dependencies.

### Circular imports (`cycles` vs knip `cycles`)

| Target | oxiclean | knip | Speed-up | Cycles (oxiclean / knip) | knip cycles oxiclean misses |
|---|---|---|---|---|---|
| Next.js | 0.94s | 307s | 326× | 68 / 44 | 0 |
| Expo | 0.75s | 180s | 240× | 46 / 51 | 0 |
| MUI | 0.51s | 105s | 208× | 4 / 6 | 0 |
| Vite | 55ms | 3.0s | 54× | 49 / 65 | 1 (knip false positive) |
| Playwright | 68ms | 2.1s | 31× | 76 / 71 | 0 |
| Lit | 74ms | 2.5s | 34× | 20 / 17 | 0 |

The cycle counts measure different things. knip reports one cycle per back
edge of its depth-first search, so a tangle yields as many cycles as the
search happens to close. oxiclean picks short cycles until removing the last
import of each leaves no cycles. The last column checks that oxiclean covers
everything knip finds: it counts knip cycles that none of oxiclean's last
imports breaks.

- **Vite's one miss is knip's mistake:** `playground/resolve/sharp-dir-nested/index.cjs`
  requires `./#/index.cjs`, a file in a directory named `#`. knip treats `#`
  as a URL fragment, resolves the import to the file itself, and reports a
  self-import.
- **Next.js has 4 cycles only oxiclean reports.** They run through
  `packages/next/src/compiled/babel`, whose files import the package's own
  `next/dist/compiled/...` paths. oxiclean resolves those to the sources,
  which are copied unchanged into `dist`; knip does not follow them.

## Choosing thresholds

The default thresholds (300 modules for `bloat`, 20 levels for
`depth`) come from the distribution of each file's worst value in
these targets, measured with `--format json` and a threshold of 1 in `.oxicleanrc.json`:

| Target | Check | Files | p90 | p95 | p99 | Files flagged at default |
|---|---|---|---|---|---|---|
| Next.js | bloat | 2,129 | 172 | 309 | 966 | 112 (5.3%) |
| Expo | bloat | 4,319 | 217 | 497 | 585 | 400 (9.3%) |
| Next.js | depth | 1,294 | 18 | 23 | 29 | 104 (8.0%) |
| Expo | depth | 3,112 | 41 | 43 | 50 | 736 (23.7%) |

The old defaults (200 and 10) flagged 9–11% of files for bloat and 22–57% for
depth, which buried the outliers. Expo's long import chains are concentrated in
`apps/native-component-list` (394 files) and `packages/expo-router` (151), which
is what per-package `[[overrides]]` are for. These numbers are without
`node_modules`; with dependencies installed, third-party modules count too and
values go up.
