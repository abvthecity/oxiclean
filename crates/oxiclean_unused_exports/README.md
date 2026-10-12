# Unused Exports Checker

Finds exports that no other file imports. It is the equivalent of knip's
`exports` and `types` issue types, as two rules:

- `unused-exports`: values, functions, classes and namespaces;
- `unused-types`: types, interfaces and enums.

```ts
// src/button.ts
export const Button = () => null; // imported by src/index.ts
export const unusedHelper = 2;    // reported by unused-exports
export type Size = 'sm' | 'lg';   // reported by unused-types
```

## Usage

```bash
oxiclean unused-exports --root ./my-project
oxiclean unused-types --root ./my-project
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

Every unused export is reported. Severity, entry files and ignore patterns
are set in `.oxicleanrc.json`:

```json
{
  "rules": { "unused-exports": "error", "unused-types": "warn" }
}
```

## Example Output

```
⚠ Unused exports detected

src/button.ts
└──  export 'unusedHelper' (never imported)
```

## Entry files

The exports of entry files are what the project offers, so they always
count as used. Without `entries` in the config, the entries are what knip
finds with its plugins off, in the root package and each workspace package:

- the files `package.json` points to with `main`, `module`, `source`,
  `browser`, `types`, `typings`, `bin` and `exports` (every condition, and
  `*` patterns). A target in build output maps to its source, whether or
  not the package has been built: through
  the package tsconfig's `outDir` and `rootDir` as in knip, or, as oxiclean
  resolves imports between workspace packages, from `dist/index.js` (or
  `build`, `lib`, `esm`, `cjs`, `out`) to `src/index.ts`;
- the JS/TS files its `scripts` run, such as `node scripts/build.js`;
- `index`, `main` and `cli` files in the package directory and its `src`;
- test files (`*.test.*`, `*.spec.*` and files in `__tests__`), which knip's
  test-runner plugins add;
- declaration files in a directory with a `tsconfig.json`, which knip takes
  from the TypeScript program;
- files passed to `require.resolve()` or `import.meta.resolve()`, which run
  in a worker or another process.

Only files these entries reach are checked. A file nothing imports is an
unused file, not a file of unused exports. With `entries` set, those files
are the only entries.

## What counts

An export is used when:

- another file imports it by name, including type-only imports, `require`
  with destructuring (`const { a } = require('./x')`) and `await import()`
  with destructuring;
- another file imports the module as a namespace and reads the export from
  it (`ns.a`, `ns['a']`, `ns.A` in a type, `<ns.A />`, or destructuring).
  A namespace used any other way, such as passed to a function, uses every
  export. `const m = require('./x')` and `const m = await import('./x')`
  use `default` and the members read from `m`, as a namespace does;
  destructuring with `...rest` and an `import('./x')` that isn't awaited use
  every export;
- `() => import('./x')` is passed to a function, as route loaders do: that
  uses the default export only;
- it is re-exported (`export { a } from './x'`, `export * from './x'`, or an
  imported name exported again) and the re-export is used. `export *`
  passes on every name except `default` that the re-exporting file doesn't
  export itself;
- a used function, class or variable refers to it in its signature, such as
  a parameter or return type;
- it is used inside `declare module './x' { ... }` in another file;
- it is the same export of a JavaScript file's `.d.ts`, or the reverse;
- it is imported in a `<script>` block of a `.vue`, `.svelte` or `.astro`
  file, or in Astro's frontmatter.

These are never reported, as in knip:

- exports of entry files, and everything they re-export;
- exports whose JSDoc has a `@public`, `@beta` or `@alias` tag;
- files inside `node_modules`;
- `export * as ns from './x'`, whose members are checked in `./x`.

Where oxiclean differs from knip:

- knip keeps every member of a namespace import that is aliased
  (`const n = ns`), spread or nested further; oxiclean keeps every export of
  such a namespace too, so it reports less, never more;
- knip reads `.mdx` files and counts `import.meta.glob` patterns, files that `fork()` or `spawn()`
  run, and the config files its plugins know; oxiclean doesn't read them
  yet, so files reached only that way are not checked;
- knip maps build output to sources only through tsconfig `outDir`, so it
  reports the re-exports of a package whose `exports` point to `dist` with
  no `outDir` (as Vite's and Playwright's do); oxiclean maps `dist` to `src`
  and treats them as the package's interface;
- knip takes declaration files from each tsconfig's `include`; oxiclean
  takes every one in a directory with a `tsconfig.json`;
- exports used only inside their own file are reported, as with knip's
  default `ignoreExportsUsedInFile: false`; there is no option to change it
  yet.

## Library Usage

```rust,no_run
use oxiclean_core::CommonArgs;
use oxiclean_unused_exports::{Config, FindingKind, run_unused_exports_check};

let result =
    run_unused_exports_check(&Config { common: CommonArgs::with_root("./my-project") })?;
for f in &result.findings {
    if let FindingKind::UnusedExport(unused) = &f.kind {
        println!("{}:{} {}", f.file.display(), unused.line, unused.name);
    }
}
# Ok::<(), oxiclean_core::Error>(())
```

## Conformance with knip

The `tests/fixtures/knip-*` projects are knip's own fixtures, copied
unchanged with its ISC license notice (`tests/fixtures/KNIP-LICENSE.md`).
oxiclean reports the same unused exports and types on them as knip's tests
expect. Across 82 of knip's export, type, import and re-export fixtures, 66
give knip's exact results; the rest are the differences above or need knip
options oxiclean doesn't have.

## License

MIT
