# Unused Exports Checker

Finds exports that no other file imports, and members of exported enums and
namespaces that nothing uses. It is the equivalent of knip's `exports`,
`types`, `enumMembers` and `namespaceMembers` issue types, as four rules:

- `unused-exports`: values, functions, classes and namespaces;
- `unused-types`: types, interfaces and enums;
- `unused-enum-members`: keys of used enums;
- `unused-namespace-members`: what used namespaces export.

```ts
// src/button.ts
export const Button = () => null; // imported by src/index.ts
export const unusedHelper = 2;    // reported by unused-exports
export type Size = 'sm' | 'lg';   // reported by unused-types
```

## Usage

```bash
oxiclean unused --root ./my-project
```

`oxiclean unused` runs the four rules from one module graph, each unless
`.oxicleanrc.json` turns it off (`"unused-types": "off"`, say).

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
- the targets of tsconfig `paths` aliases named after a workspace package,
  such as `"@mui/icons-material/*": ["./packages/mui-icons-material/lib/*"]`,
  since imports of the package resolve through the alias;
- `index`, `main` and `cli` files in the package directory and its `src`;
- JS/TS config files in the package directory, such as `vite.config.ts`
  or `.eslintrc.cjs`, which knip's plugins take as entries;
- test files (`*.test.*`, `*.spec.*` and files in `__tests__`), which knip's
  test-runner plugins add;
- HTML pages that load the project's scripts (a `<script>` with a local
  `src`, or an inline module script), as a Vite app's `index.html` does,
  which knip's Vite plugin adds;
- the files plugins make entries (see [Plugins](../../README.md#plugins)):
  Next.js pages, App Router files and middleware, Expo Router routes,
  Storybook stories and `.storybook` files, and the setup files Jest's and
  Vitest's configs name;
- declaration files in a directory with a `tsconfig.json`, which knip takes
  from the TypeScript program;
- files passed to `require.resolve()` or `import.meta.resolve()`, or named
  by `new URL('./x', import.meta.url)`, which run in a worker or another
  process.

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
- `import.meta.glob` matches the file: that uses every export, or the one
  its `import` option names (`{ import: 'default' }`);
- it is re-exported (`export { a } from './x'`, `export * from './x'`, or an
  imported name exported again) and the re-export is used. `export *`
  passes on every name except `default` that the re-exporting file doesn't
  export itself;
- a used function, class or variable refers to it in its signature, such as
  a parameter or return type;
- it is used inside `declare module './x' { ... }` in another file;
- it is the same export of a JavaScript file's `.d.ts`, or the reverse;
- it is imported in a `<script>` block of a `.vue`, `.svelte`, `.astro` or
  `.html` file, in Astro's frontmatter, or by an `import` line of an `.mdx`
  file.

These are never reported, as in knip:

- exports of entry files, and everything they re-export;
- exports whose JSDoc has a `@public`, `@beta` or `@alias` tag;
- files inside `node_modules`;
- `export * as ns from './x'`, whose members are checked in `./x`.

Where oxiclean differs from knip:

- knip keeps every member of a namespace import that is aliased
  (`const n = ns`), spread or nested further; oxiclean keeps every export of
  such a namespace too, so it reports less, never more;
- knip counts `import.meta.glob` patterns, files that `fork()` or `spawn()`
  run, and the config files its plugins know in other formats; oxiclean doesn't read them
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

## Unused members

`unused-enum-members` and `unused-namespace-members` check the members of
exported enums and TypeScript namespaces that something uses. As in knip,
only those exported where they are declared (`export enum E {}`) are
checked, not ones exported by name (`export { E }`). An unused enum is
reported by `unused-types`, not member by member. A namespace's
members are what it exports; members of a nested namespace are named by
their path, as `Fruits.Tropical.mango`. When an enum and a namespace of the
same name merge, all their members are enum members, as in knip.

```ts
// src/lib.ts
export enum Color { Red, Blue }   // Color.Blue is reported
export namespace Api {
  export const get = 1;
  export function put() {}        // Api.put is reported
}

// src/index.ts (an entry)
import { Color, Api } from './lib';
console.log(Color.Red, Api.get);
```

A member is used when:

- it is read on the export, in values or types, through imports, renames
  and re-exports: `Color.Red`, `Color['Red']`, `ns.Color.Red`,
  `let c: Color.Red`, `Fruits.Tropical.mango`. Reading a member's own
  members uses it too;
- it is read in its own file, by path or, inside its namespace, by name. A
  local that shadows the export, such as a parameter called `Color`,
  doesn't count;
- every member is read at once: `Color[key]`, a numeric key such as
  `Color[3]` or `Color['3']`, `Object.keys(Color)` (or `values`, `entries`,
  `getOwnPropertyNames`), `const c = Color`, the type `Color` or
  `typeof Color`;
- the export is re-exported by an entry file, or its module is used as a
  whole.

Passing the export around, as in `f(Color)`, uses none of its members, as
in knip. Members tagged `@public`, `@beta` or `@alias` are never reported,
and neither are the members of exports with those tags.

Where oxiclean differs from knip:

- knip follows a local alias (`const c = Color; c.Red`) member by member;
  oxiclean counts every member of an aliased export as used;
- a deeper path read as a whole on a named import, as `N.Inner[key]`,
  counts as reading all of `N`;
- members of ambient namespaces (`declare namespace`, or any namespace in a
  `.d.ts` file) are not checked, since everything in them is exported
  whether or not it says so;
- a local that shadows an imported name is only told apart in the file that
  declares the members, as for the other checks.

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
oxiclean reports the same unused exports, types and members on them as
knip's tests expect. Across 82 of knip's export, type, import and re-export fixtures, 66
give knip's exact results; the rest are the differences above or need knip
options oxiclean doesn't have. All 14 of knip's enum and namespace member
fixtures in `types`, `namespaces`, `entry` and `re-exports` give knip's
results with its default options.

## License

MIT
