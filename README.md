# Oxiclean 🧼

A Rust monorepo for code quality and analysis tools for JavaScript/TypeScript projects.

## Quick Start

```bash
# Install
cargo install --path crates/oxiclean

# Run import bloat checker
oxiclean bloat --root ./my-project

# Run import depth checker
oxiclean depth --root ./my-project

# Find barrel files that pull in too many modules
oxiclean barrels --root ./my-project

# Find circular imports
oxiclean cycles --root ./my-project

# Find modules that export the same thing twice
oxiclean duplicates --root ./my-project

# Find exports and exported types that nothing imports
oxiclean unused-exports --root ./my-project
oxiclean unused-types --root ./my-project

# Find members of used enums and namespaces that nothing uses
oxiclean unused-enum-members --root ./my-project
oxiclean unused-namespace-members --root ./my-project

# Find package.json dependencies and devDependencies that nothing imports or runs
oxiclean dependencies --root ./my-project

# Find pnpm catalog entries that no package.json refers to
oxiclean catalog --root ./my-project

# Find installed packages whose import pulls in too many modules
oxiclean heavy-dependencies --root ./my-project

# Find packages the lockfile installs at more than one version
oxiclean duplicate-versions --root ./my-project

# Get help
oxiclean --help
oxiclean bloat --help
oxiclean depth --help
oxiclean cycles --help
oxiclean duplicates --help
oxiclean unused-exports --help
```

## Development

```bash
cargo build                                       # build
cargo run -- bloat --root ./my-project     # run the CLI from source
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for tests, linting and benchmarks.

## Project Structure

```
oxiclean/
├── crates/
│   ├── oxiclean/                    # Main CLI application (binary)
│   ├── oxiclean_core/               # Shared utilities (parser, resolver, etc.)
│   ├── oxiclean_bloat/              # Import bloat detection library
│   ├── oxiclean_cycles/             # Circular import detection library
│   ├── oxiclean_dependencies/       # Unused dependency detection library
│   ├── oxiclean_duplicates/         # Duplicate export detection library
│   ├── oxiclean_unused_exports/     # Unused export and type detection library
│   └── oxiclean_depth/              # Import depth analysis library
└── Cargo.toml                       # Workspace configuration
```

## Tools

The command line only says where to look and how to print. Everything else,
including thresholds, entry files and ignore patterns, is set in
[`.oxicleanrc.json`](#configuration).

| Option | Meaning |
|---|---|
| `--root <DIR>` | Project root. Defaults to the git root of the current directory. |
| `-c, --config <FILE>` | Read this config file instead of `.oxicleanrc.json` in the root. |
| `--format <text\|json\|sarif>` | Output format (see [Output formats](#output-formats)). |

The default thresholds are 300 modules for `bloat`, 25 modules for `barrels`, 100 modules for `heavy-dependencies`, 2 versions for `duplicate-versions` and 20 levels for `depth`. They flag roughly the worst 5% of files in Next.js and the worst 10–25% in Expo (see [bench/README.md](bench/README.md#choosing-thresholds)).

`cycles` reports every cycle, so its threshold (the number of files in a cycle) defaults to 1. `duplicates` reports every export with a second name, so its threshold (the number of names) defaults to 2. `unused-exports`, `unused-types`, `unused-enum-members`, `unused-namespace-members`, `dependencies` and `catalog` report every unused export, member, package or catalog entry and have no threshold.

Every check exits with status 1 when a finding with `error` severity crosses its threshold, so they can gate CI. Findings with `warn` severity are reported but don't fail the run. Files that fail to parse are listed at the end of the report.

### Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes. Counts the total number of unique modules reachable from import statements (breadth-first analysis).

**Usage:**
```bash
oxiclean bloat --root ./my-project
```

**Example Output:**
```
⚠ Import bloat detected (threshold: 300 modules)

src/pages/Dashboard.tsx (512 modules)
├──  import './components/DataTable' (408 modules)
└──  import '@/utils/api' (331 modules)
```

**Features:**
- Resolves Node.js modules and TypeScript path mappings
- Follows re-exports (`export * from`) so barrel files are counted
- Skips dynamic `import()` unless `includeDynamic` is set
- Respects `.gitignore` patterns, even outside a git repository
- Skips test files, `node_modules` and `.d.ts` declarations

### Barrel Files Checker

Finds barrel files, modules that mostly re-export others (`export * from`,
`export { a } from`), whose import pulls in at least `threshold` modules
(default 25), itself included. A module is a barrel when it forwards at
least two modules and has fewer exports of its own than forwarding exports.
Only runtime imports count, so `export type` re-exports pull in nothing.
Each finding names how many modules the barrel re-exports from and how many
files import it; importing the module that defines a name directly skips
the rest.

**Usage:**
```bash
oxiclean barrels --root ./my-project
```

**Example Output:**
```
⚠ Heavy barrel files detected (threshold: 25 modules)

packages/ui/src/index.ts
└──  barrel re-exporting 142 modules, imported by 75 files (pulls in 809 modules)
```

### Import Depth Checker

Detects excessive import depth in JavaScript/TypeScript projects that can lead to slow module resolution and complex dependency chains. Measures the longest chain of imports from an entry point (depth-first analysis).

**Usage:**
```bash
oxiclean depth --root ./my-project
```

**Example Output:**
```
⚠ Excessive import depth detected (threshold: 20 levels)

src/pages/Dashboard.tsx
├──  import './components/DataTable' (depth: 26)
└──  import '@/utils/format' (depth: 21)
```

**Features:**
- Computes the longest import chain on the module graph. Modules that import each other in a cycle count as one level, so results are the same on every run
- Correctly handles TypeScript type imports:
  - `import type { Foo }` - Ignored (type-only)
  - `import { type Foo }` - Ignored when every imported name is a type
- Resolves Node.js modules and TypeScript path mappings
- Handles very deep chains without recursion

### Import Cycles Checker

Finds files that import each other, directly or indirectly, at runtime, like
knip's `cycles` issue type. Type-only imports never count. Dynamic
`import()` counts only when `includeDynamic` is set; unlike knip's
`cycles.dynamicImports`, that setting applies to every check. knip's
`cycles.allow` has no equivalent yet; use `ignorePatterns` instead.

**Usage:**
```bash
oxiclean cycles --root ./my-project
```

**Example Output:**
```
⚠ Circular imports detected (threshold: 1 files)

src/store/index.ts
└──  import './slices' (cycle of 3 files)
        src/store/index.ts:4 imports './slices'
        src/store/slices.ts:2 imports './user'
        src/store/user.ts:1 imports './index'
        part of a tangle of 7 files that import each other
```

**How cycles are chosen:** a tangle of files that import each other can
contain many overlapping cycles. Oxiclean uses a port of pnpm's
[graph-sequencer](https://github.com/pnpm/pnpm/tree/main/pnpm11/deps/graph-sequencer)
to pick short cycles: for each file it searches from, the longest of the
shortest cycles through it. The import that closes each reported cycle is
then set aside and the search repeats, so removing the last import of each
reported cycle leaves no cycles. Each cycle is reported on its first file by
path that the rule applies to, so `ignorePatterns` and `overrides` hide a
cycle only when they cover every file in it. The result is the same on every
run. JSON output
lists every import of the cycle, and SARIF output links to each of them.

### Duplicate Exports Checker

Finds modules that export the same thing under more than one name, like
knip's `duplicates` issue type: `export const b = a` or `export default a`
where `a` is already exported. Importers can then reach one value through
two names. It checks every parsed file outside `node_modules`, including
declaration files: those imported for their types, and the `.d.ts` next to
a `.js` file.

As in knip, these don't count: a name with an `@alias` tag in the comment
before it, re-exports and imports from the project passed on as the original, renames with
`export { a as b }`, and `export default a` when `a` is exported with
`export { a }`. Unlike knip, the original export may come after the name
that repeats it. See the [checker's README](crates/oxiclean_duplicates/README.md#what-counts)
for every case.

**Usage:**
```bash
oxiclean duplicates --root ./my-project
```

**Example Output:**
```
⚠ Duplicate exports detected (threshold: 2 names)

src/button.ts
└──  export 'Button' (exported as 2 names)
        src/button.ts:12 also exports it as 'default'
```

### Unused Exports and Types Checkers

Find exports that no other file imports, like knip's `exports` and `types`
issue types. `unused-exports` reports values, functions, classes and
namespaces; `unused-types` reports types, interfaces and enums. Exports
tagged `@public` (or `@beta`, or `@alias`) in their JSDoc are never
reported, as in knip.

The exports of entry files are the project's interface and always count as
used, so these checks don't take every file in `src` as an entry. Unless
`entries` is set, they start from what each package offers, as knip does:
the files its `package.json` points to (`main`, `exports`, `bin` and
others) or runs in `scripts`, its `index`, `main` and `cli` files, test
files, and declaration files. Files no entry reaches aren't checked. See the
[checker's README](crates/oxiclean_unused_exports/README.md) for what counts
as a use and where oxiclean differs from knip.

**Usage:**
```bash
oxiclean unused-exports --root ./my-project
oxiclean unused-types --root ./my-project
```

**Example Output:**
```
⚠ Unused exports detected

src/button.ts
└──  export 'unusedHelper' (never imported)
```

### Unused Enum and Namespace Members Checkers

Find the members of exported enums and TypeScript namespaces that nothing
uses, like knip's `enumMembers` and `namespaceMembers`. Only exports that
something uses are checked; an export nothing uses is reported by
`unused-exports` or `unused-types` instead. `E.a`, `E['a']` and `N.a.b`
use a member, in values and types and in the member's own file, while
`E[key]`, `Object.keys(E)` and the type `E` use them all. Members tagged
`@public` are never reported.

```ts
// src/color.ts
export enum Color {
  Red,  // used: src/index.ts reads Color.Red
  Blue, // reported by unused-enum-members
}
```

**Usage:**
```bash
oxiclean unused-enum-members --root ./my-project
oxiclean unused-namespace-members --root ./my-project
```

### Unused Dependencies Checker

Find the packages a `package.json` lists in `dependencies` or
`devDependencies` that no file imports and no script runs, like knip's
`dependencies` and `devDependencies`, as one rule whose `fields` option
picks `dependencies`, `devDependencies` or both (the default), per package
too with `overrides`. The root's and every workspace
package's `package.json` are checked, from the files the package entries
reach (including config files such as `vite.config.ts`), the binaries their
`scripts` run, and the packages their tsconfig files name. `@types/x` is
used when `x` is imported. Packages in `ignoreDependencies` are never
reported. See
[oxiclean_dependencies](crates/oxiclean_dependencies/README.md) for what
counts as a use.

**Usage:**
```bash
oxiclean dependencies --root ./my-project
```

### Heavy Dependencies Checker

Finds the packages a `package.json` lists whose import pulls in at least
`threshold` modules (default 100), counted through `node_modules`, so the
packages must be installed. Every entry point of a package the project
imports (`date-fns`, `@scope/pkg/sub`) counts together, with the packages
those import in turn; only runtime imports count. Findings point at the
package's line in the `package.json` that lists it and say how many files
import it.

**Usage:**
```bash
oxiclean heavy-dependencies --root ./my-project
```

**Example Output:**
```
⚠ Heavy dependencies detected (threshold: 100 modules)

package.json
└──  package 'date-fns', imported by 12 files (pulls in 302 modules)
```

### Duplicate Package Versions Checker

Finds the packages `package-lock.json` (v2 and v3) or `pnpm-lock.yaml` (v6
and v9) installs at more than one version, at or above `threshold`
versions (default 2). Each finding lists the versions, lowest first, and
points at the package's first entry in the lockfile. pnpm's peer variants
of one version count once.

**Usage:**
```bash
oxiclean duplicate-versions --root ./my-project
```

**Example Output:**
```
⚠ Duplicate package versions detected (threshold: 2 versions)

pnpm-lock.yaml
├──  package 'semver' (5.7.2, 6.3.1, 7.7.2, 7.7.4) (4 versions)
└──  package 'minimatch' (3.1.5, 9.0.9) (2 versions)
```

### Unused Catalog Entries Checker

Find the entries of pnpm's `catalog` and `catalogs` in `pnpm-workspace.yaml`
that no `package.json` in the workspace refers to with `catalog:` or
`catalog:<name>`, and no `overrides` entry uses, like knip's `catalog`.
Findings point at the entry's line in `pnpm-workspace.yaml`.

**Usage:**
```bash
oxiclean catalog --root ./my-project
```

### Difference Between Tools

- **Import Bloat**: Counts total reachable modules (breadth) - indicates bundle size impact
- **Import Depth**: Counts longest import chain (depth) - indicates module resolution complexity
- **Import Cycles**: Lists files that import each other - order-dependent initialisation bugs and code that can't be split
- **Duplicate Exports**: Lists exports with a second name - two ways to import one thing
- **Unused Exports and Types**: Lists exports nothing imports - dead code and API surface to remove
- **Unused Enum and Namespace Members**: Lists members of used enums and namespaces that nothing reads - dead cases and helpers
- **Unused Dependencies**: Lists packages a package.json lists that nothing imports or runs - installs to drop

These metrics provide complementary insights into code complexity.

## Configuration

Put a `.oxicleanrc.json` in the project root. Its format follows oxlint's
[`.oxlintrc.json`](https://oxc.rs/docs/guide/usage/linter/config.html):
comments and trailing commas are allowed, each rule is set to a severity
(`"off"`, `"warn"` or `"error"`) or to `[severity, options]`, and `overrides`
apply settings to some files. Every key is optional.

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/abvthecity/oxiclean/main/configuration_schema.json",
  // Files that are never entries and never reported, relative to the root
  "ignorePatterns": ["**/fixtures/**", "apps/playground/**"],
  // Entry files. Without this, every JS/TS file inside a `src` directory is
  // an entry, except for the unused export and member checks, which start
  // from each package's entry points. `*` stays within one directory and `**`
  // crosses directories.
  "entries": ["packages/*/src/**/*.{ts,tsx}"],
  // Turn plugins on or off; see "Plugins" below
  "plugins": { "html": true, "mdx": false },
  // Packages the unused dependency checks never report
  "ignoreDependencies": ["eslint-config-*"],
  // Also follow dynamic import() calls
  "includeDynamic": false,
  // Also scan installed packages: parse files in node_modules and follow
  // their imports. Off by default, so each imported package file counts as
  // one module.
  "includeNodeModules": false,
  "rules": {
    // "warn" reports without failing the run; "error" fails it
    "bloat": ["error", { "threshold": 300, "ignorePatterns": ["scripts/**"] }],
    "depth": "off",
    // Only report cycles of 3 or more files
    "cycles": ["warn", { "threshold": 3 }]
  },
  // Later overrides win. `packages` matches workspace package names (globs
  // allowed) and `files` matches paths relative to the root; when both are
  // given, a file must match both.
  "overrides": [
    {
      "packages": ["@acme/legacy-*"],
      "rules": { "bloat": ["warn", { "threshold": 1000 }] }
    },
    {
      "packages": ["@acme/web"],
      "rules": { "depth": ["error", { "threshold": 12 }] }
    }
  ]
}
```

Unknown keys and rule names are errors, so a typo can't silently do nothing.
A rule listed with only a severity keeps its default threshold.

[`configuration_schema.json`](configuration_schema.json) is a JSON schema for
the file, generated from the Rust types; point `$schema` at it for completion
and validation in your editor. After changing the config types, regenerate it
with `UPDATE_SCHEMA=1 cargo test -p oxiclean_core committed_schema`; a test
fails while the committed schema is out of date.

## Plugins

Plugins teach oxiclean about file formats and tools around JS/TS, as knip's
plugins do. Every check reads through them, so an import in an MDX page
counts toward bloat, depth and cycles as well as unused dependencies. A
plugin can parse a format into imports, make files entries, and name the
packages a tool's config files use.

| Plugin | What it reads | On by default |
|---|---|---|
| `html` | `<script>` blocks and `<script src>`; pages that load the project's scripts are entries | always |
| `mdx` | `import` and `export` lines of `.mdx` files | always |
| `vue`, `svelte`, `astro` | components' `<script>` blocks, and Astro's frontmatter | always |
| `css` | `@import`, `@use`, `@forward`, `@plugin`, `@config` and `@reference` in CSS, Sass, Less and PostCSS files | always |
| `typescript` | packages tsconfig files name in `extends`, `types`, `jsxImportSource` and `plugins` | always |
| `eslint` | packages `.eslintrc*` files and `eslintConfig` name in `extends`, `plugins`, `parser` and import resolvers | always |
| `babel` | presets and plugins Babel's config files and `babel` name | always |
| `postcss` | plugins and syntaxes PostCSS's config files and `postcss` name | always |
| `prettier` | the shared config and plugins Prettier's config files and `prettier` name | always |
| `jest` | presets, environments, transforms and reporters Jest's config names; its setup files are entries | when `jest` is listed |
| `vitest` | environments, reporters and coverage providers in the `test` options of Vitest's or Vite's config; its setup files are entries | when `vitest` is listed |
| `next` | pages, App Router files (`page`, `layout`, `route`...), middleware and instrumentation are entries | when `next` is listed |
| `expo` | config plugins in `app.json` and `app.config.*` and a package `main` are used; Expo Router's `app` routes are entries | when `expo` is listed |
| `storybook` | addons and framework `.storybook/main.*` names are used; stories and `.storybook`'s `main`, `preview` and `manager` are entries | when `storybook` is listed |
| `husky` | commands in `.husky` hooks and `simple-git-hooks`, read as scripts | when `husky` or `simple-git-hooks` is listed |
| `lint-staged` | commands lint-staged's config runs, read as scripts | when `lint-staged` is listed |

Tool plugins read JSON configs and the object a JS/TS config exports,
including one a function returns, without running them; YAML configs
aren't read yet. Plugins that read a config file are on whenever the file
is there; those that need a tool's package to be listed say so. `"plugins": { "html": false }` in `.oxicleanrc.json` turns a plugin
off, and `true` turns one on.

## Output formats

`--format text` (the default) prints the report shown above.

`--format json` prints one JSON document with a `summary` (files analysed,
error and warning counts, import resolution) and every finding with its
`rule`, `severity`, `file` (relative to the root, with `/`), `line`,
`package`, `request`, `resolved`, `value`, `threshold` and a `message`.

`--format sarif` prints a [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
log, which GitHub code scanning and other tools can show inline on pull requests:

```yaml
- run: oxiclean bloat --format sarif > oxiclean.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: oxiclean.sarif
```

Each rule's log has its own code-scanning category (`oxiclean/<rule>/`), so
uploading several rules for one commit keeps every rule's alerts. Results
carry `partialFingerprints` that ignore line numbers, so an alert stays the
same alert when code above it moves.

## Debugging & Logging

Oxiclean logs diagnostics with [`tracing`](https://docs.rs/tracing) to stderr, so stdout carries only the report. Enable logging via the `RUST_LOG` environment variable (errors only by default):

```bash
# Show all debug logs
RUST_LOG=debug oxiclean depth

# Show only info level
RUST_LOG=info oxiclean bloat

# Show trace level (very verbose)
RUST_LOG=trace oxiclean depth

# Filter by module
RUST_LOG=oxiclean_core::resolver=trace oxiclean depth
```

## TypeScript Path Aliases

Both tools fully support TypeScript path aliases defined in `tsconfig.json`:

```json
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"],
      "~lib/*": ["./lib/*"]
    }
  }
}
```

Imports like `import { foo } from '@/utils'` are correctly resolved and analyzed.
Each file uses the aliases of its nearest `tsconfig.json` (following `extends`),
so packages in a monorepo can define the same alias differently.

## Monorepos

Workspace packages declared in `pnpm-workspace.yaml`, the root `package.json`
`workspaces` field (npm, yarn and bun) or, failing those, `lerna.json` resolve
to their source files, even before `node_modules` is installed. When a package's `main` or `exports` points at build output that
doesn't exist yet (for example `build/index.js`), the matching file under
`src/` is used instead.

Use `overrides` with `packages` in `.oxicleanrc.json` to give packages their
own settings. JSON and SARIF output name the package each finding belongs to.

## What counts as an import

Static `import`, re-exports (`export * from`, `export { x } from`), `require()`
calls anywhere in a file, and TypeScript `import x = require()` are followed.
Type-only imports and exports are ignored. Dynamic `import()` is not followed,
because it loads the module as a separate chunk, which is the usual fix for
bloat. Node.js built-ins such as `fs` or `node:path` are skipped. The
`<script>` blocks of `.vue`, `.svelte`, `.astro` and `.html` files, Astro's
frontmatter, and the `import` and `export` lines of `.mdx` files are read like
any JS/TS file, and an HTML page's `<script src>` is an import. A `src`
starting with `/` is relative to the page's package, as Vite serves it.
Vite's `import.meta.glob('./pages/*.ts')` imports every file it matches,
statically with `eager: true` and dynamically otherwise; patterns starting
with `/` are relative to the package, and aliases aren't expanded.
`new URL('./worker.ts', import.meta.url)`, as `new Worker()` takes, and
`require.resolve()` find a file without loading it: the checks of exports
and dependencies follow them, the others don't.

## License

MIT
