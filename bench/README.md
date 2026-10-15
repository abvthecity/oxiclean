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
A check is one rule: the scripts run its command (`oxiclean unused` for
`unused-types`) with a temporary config file that turns the command's other
rules off, so each rule is timed alone. A run that exits with anything but
0 or 1, or in which the rule couldn't run, fails the script.

## Current baseline

Recorded without installing dependencies (`installed: false`) on a 4-core
Linux machine. `duplicates` also analyses declaration files and
files reached only through type imports, so it sees more files. `unused-exports` and `unused-types` start from package entry points
and read declaration files too; MUI's tsconfig aliases make its built icon
modules entries, so they see twice as many files there. Imports between workspace packages resolve to their sources;
third-party packages such as `react` do not, which accounts for most of the
unresolved imports.

| Target | Check | Time | Files | Violations | Imports resolved |
|---|---|---|---|---|---|
| Next.js | bloat | ~1.2s | 2,344 | 164 | 88.7% |
| Next.js | depth | ~1.1s | 2,344 | 202 | 88.7% |
| Next.js | cycles | ~1.2s | 2,344 | 68 | 88.7% |
| Next.js | duplicates | ~1.3s | 2,415 | 10 | 88.5% |
| Next.js | unused-exports | ~1.3s | 4,372 | 629 | 86.9% |
| Next.js | unused-types | ~1.2s | 4,372 | 305 | 86.9% |
| Expo | bloat | ~1.0s | 4,656 | 868 | 74.7% |
| Expo | depth | ~0.9s | 4,656 | 1,586 | 74.7% |
| Expo | cycles | ~0.8s | 4,656 | 46 | 74.7% |
| Expo | duplicates | ~1.0s | 4,686 | 18 | 75.4% |
| Expo | unused-exports | ~1.1s | 5,875 | 522 | 74.8% |
| Expo | unused-types | ~1.1s | 5,875 | 265 | 74.8% |
| MUI | bloat | ~0.7s | 13,301 | 690 | 68.3% |
| MUI | depth | ~0.8s | 13,301 | 3,788 | 68.3% |
| MUI | cycles | ~0.7s | 13,301 | 4 | 68.3% |
| MUI | duplicates | ~1.0s | 13,583 | 3 | 69.0% |
| MUI | unused-exports | ~1.8s | 26,873 | 15 | 62.3% |
| MUI | unused-types | ~1.4s | 26,873 | 8 | 62.3% |
| Vite | bloat | ~60ms | 383 | 0 | 72.9% |
| Vite | depth | ~50ms | 383 | 0 | 72.9% |
| Vite | cycles | ~50ms | 383 | 49 | 72.9% |
| Vite | duplicates | ~60ms | 385 | 1 | 69.3% |
| Vite | unused-exports | ~0.1s | 1,042 | 79 | 65.8% |
| Vite | unused-types | ~0.1s | 1,042 | 28 | 65.8% |
| Playwright | bloat | ~60ms | 765 | 75 | 87.6% |
| Playwright | depth | ~70ms | 765 | 53 | 87.6% |
| Playwright | cycles | ~60ms | 765 | 76 | 87.6% |
| Playwright | duplicates | ~90ms | 804 | 6 | 90.5% |
| Playwright | unused-exports | ~0.1s | 1,446 | 110 | 89.9% |
| Playwright | unused-types | ~0.1s | 1,446 | 221 | 89.9% |
| Lit | bloat | ~80ms | 753 | 0 | 55.2% |
| Lit | depth | ~90ms | 753 | 0 | 55.2% |
| Lit | cycles | ~90ms | 753 | 20 | 55.2% |
| Lit | duplicates | ~0.1s | 757 | 2 | 56.0% |
| Lit | unused-exports | ~80ms | 477 | 47 | 69.5% |
| Lit | unused-types | ~90ms | 477 | 18 | 69.5% |

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
| Next.js | 1.18s | 379s | 321× | 68 / 44 | 0 |
| Expo | 0.74s | 207s | 281× | 46 / 51 | 0 |
| MUI | 0.54s | 112s | 209× | 4 / 6 | 0 |
| Vite | 53ms | 3.1s | 58× | 49 / 65 | 1 (knip false positive) |
| Playwright | 70ms | 2.2s | 31× | 76 / 71 | 0 |
| Lit | 73ms | 3.0s | 40× | 20 / 17 | 0 |

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

### Duplicate exports (`duplicates`, same name in knip)

| Target | oxiclean | knip | Speed-up | Findings (oxiclean / knip) | knip only | oxiclean only |
|---|---|---|---|---|---|---|
| Next.js | 1.15s | 380s | 332× | 10 / 10 | 0 | 0 |
| Expo | 0.78s | 222s | 286× | 18 / 18 | 0 | 0 |
| MUI | 0.63s | 111s | 176× | 3 / 3 | 0 | 0 |
| Vite | 55ms | 3.0s | 55× | 1 / 1 | 0 | 0 |
| Playwright | 92ms | 2.3s | 25× | 6 / 5 | 0 | 1 |
| Lit | 95ms | 2.8s | 30× | 2 / 4 | 2 | 0 |

A finding is one group of names for the same export; it matches when both
tools report the same names in the same file. Declaration files count for
both: MUI's `useAutocomplete.d.ts` and Playwright's `types/test.d.ts` match.
Every difference comes from which files each tool looks at, not from how it
reads them:

- **Playwright's `utils/third_party/extractZip.ts` is outside `src`,** so
  knip's `project` leaves it out; oxiclean checks every file an entry reaches.
- **Lit's two knip-only findings are in one `tsserver-plugin` test file** that
  imports `lit`, which oxiclean resolves to the workspace package's sources.
  An import from the project that is exported again is a re-export, which
  knip doesn't count; knip treats `lit` as an external package, so its
  exports count.

### Unused exports and types (`unused-exports`/`unused-types` vs knip `exports`/`types`)

Here both tools use their own default entries: package.json entry points,
`index`/`main`/`cli` files, test files and declaration files (see the
[check's README](../crates/oxiclean_unused_exports/README.md#entry-files)),
with knip's plugins off.

| Target | Check | oxiclean | knip | Speed-up | Findings (oxiclean / knip) | knip only | oxiclean only |
|---|---|---|---|---|---|---|---|
| Next.js | exports | 1.1s | 494s | ~440× | 629 / 1,094 | 529 | 64 |
| Next.js | types | 1.1s | 552s | ~490× | 305 / 305 | 27 | 27 |
| Expo | exports | 1.1s | 274s | ~250× | 522 / 805 | 311 | 28 |
| Expo | types | 1.1s | 285s | ~250× | 265 / 456 | 205 | 14 |
| MUI | exports | 1.8s | 117s | ~65× | 15 / 27 | 17 | 5 |
| MUI | types | 1.4s | 114s | ~80× | 8 / 35 | 29 | 2 |
| Vite | exports | 91ms | 3.7s | ~40× | 79 / 142 | 63 | 0 |
| Vite | types | 95ms | 3.7s | ~40× | 28 / 235 | 207 | 0 |
| Playwright | exports | 134ms | 3.2s | ~24× | 110 / 280 | 171 | 1 |
| Playwright | types | 138ms | 2.8s | ~20× | 221 / 255 | 36 | 2 |
| Lit | exports | 83ms | 2.7s | ~32× | 47 / 28 | 2 | 21 |
| Lit | types | 82ms | 2.8s | ~35× | 18 / 17 | 2 | 3 |

A finding matches when both tools report the same name in the same file.
Most differences come from which files are entries:

- **knip maps build output to sources only through tsconfig `outDir`.**
  oxiclean also maps `dist`, `build`, `lib`, `esm`, `cjs` and `out` to `src`,
  as it does when resolving imports between workspace packages, so a package
  whose `main` or `exports` point to `dist` keeps its public interface. knip
  instead checks those files as internal and reports what the index
  re-exports. This accounts for most knip-only findings in Vite (with the
  mapping turned off, 5 exports and 12 types differ), Playwright, Next.js
  and Expo.
- **knip reads more kinds of entry:** `.mdx` files, `import.meta.glob`,
  files run by `fork()` or `spawn()`, and declaration files from each
  tsconfig's `include`.
- **Most of Lit's oxiclean-only findings are in fixture and example
  packages** (`labs/test-projects`, `localize/examples`), which oxiclean
  checks and knip, from its workspace list, does not.

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
