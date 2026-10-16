# Oxiclean 🧼

A Rust monorepo for code quality and analysis tools for JavaScript/TypeScript projects.

## Quick Start

```bash
# Install
cargo install --path crates/oxiclean

# Find exports, types and enum and namespace members that nothing uses
oxiclean unused --root ./my-project

# Find unused packages (errors), heavy packages (warnings), unused pnpm catalog
# entries and packages the lockfile installs at more than one version
oxiclean dependencies --root ./my-project

# Find tsconfig setups that slow type checking: build output and overlapping
# programs, project references, slow compiler options, @types, declarations,
# large literals and huge declaration imports
oxiclean typecheck --root ./my-project

# Find imports and barrel files that pull in too many modules
oxiclean bloat --root ./my-project

# Find excessive import depth, circular imports, and modules that export the
# same thing twice
oxiclean depth --root ./my-project
oxiclean cycles --root ./my-project
oxiclean duplicates --root ./my-project

# Read a trace tsc already wrote (tsc --generateTrace trace): slow spans of
# type checking and packages loaded from more than one directory
oxiclean trace trace --root ./my-project
# Find ESLint and oxlint configs that slow linting: type-aware parsing of
# JavaScript, slow import rules, unignored build output
oxiclean lint --root ./my-project

# Apply the fixes the findings offer
oxiclean typecheck --fix --root ./my-project

# Run every command, score the project's health and rank what they found by
# the time fixing it saves developers and CI each week
oxiclean health --root ./my-project

# Explore which tsconfig programs type-check each package, and why, in an
# interactive view (or print the map with --format json)
oxiclean map --root ./my-project

# Get help
oxiclean --help
oxiclean unused --help
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
│   ├── oxiclean_lint/               # ESLint and oxlint config checks
│   ├── oxiclean_typecheck/          # tsconfig and lint setup checks
│   ├── oxiclean_trace/              # tsc --generateTrace analysis
│   ├── oxiclean_tui/                # Interactive terminal views (oxiclean map)
│   ├── oxiclean_unused_exports/     # Unused export and type detection library
│   └── oxiclean_depth/              # Import depth analysis library
└── Cargo.toml                       # Workspace configuration
```

## Tools

Each command runs a group of rules, and `.oxicleanrc.json` turns each rule
on or off. A rule that is `"off"` doesn't run at all, so turning off
`heavy-dependencies` keeps `oxiclean dependencies` from parsing
`node_modules`. Rule names in the config file are the names below.

| Command | Rules |
|---|---|
| `unused` | `unused-exports`, `unused-types`, `unused-enum-members`, `unused-namespace-members` |
| `dependencies` | `dependencies`, `heavy-dependencies`, `catalog`, `duplicate-versions` |
| `typecheck` | `program-files`, `overlap`, `references`, `tsconfig`, `ambient-types`, `isolated-declarations`, `large-literals`, `type-bloat`, `complex-types` |
| `bloat` | `bloat`, `barrels` |
| `depth` | `depth` |
| `cycles` | `cycles` |
| `duplicates` | `duplicates` |
| `trace [DIR]` | `hot-spots`, `duplicate-packages` |
| `lint` | `lint-config` |
| `health` | every rule above (the `trace` rules only when `trace.dir` is set) |
| `map` | none: an interactive view (see [Program map](#program-map)) |

Every rule is on by default with `error` severity, except
`heavy-dependencies`, `hot-spots`, `duplicate-packages` and `lint-config`,
which default to `warn`: a heavy package is worth a look but may be worth
its weight, trace timings vary from run to run, and a slow lint setup costs
time but nothing else. A severity in the config file wins over
the default. Rules of one command share their work where they can: the
`unused` rules share one module graph and one pass over its uses, `bloat`
and `barrels` share a graph, and the `typecheck` rules read the tsconfig
files once.

The command line only says where to look and how to print. Everything else,
including thresholds, entry files and ignore patterns, is set in
[`.oxicleanrc.json`](#configuration).

| Option | Meaning |
|---|---|
| `--root <DIR>` | Project root. Defaults to the git root of the current directory. |
| `-c, --config <FILE>` | Read this config file instead of `.oxicleanrc.json` in the root. |
| `--format <text\|json\|sarif>` | Output format (see [Output formats](#output-formats)). |
| `--fix` | Apply the fixes findings offer, then report what is left (see [Fixes](#fixes)). |

The default thresholds are 300 modules for `bloat`, 25 modules for `barrels`, 100 modules for `heavy-dependencies`, 2 versions for `duplicate-versions` and 20 levels for `depth`. They flag roughly the worst 5% of files in Next.js and the worst 10–25% in Expo (see [bench/README.md](bench/README.md#choosing-thresholds)). `type-bloat` defaults to 100000 lines of declarations, about a quarter of a second of parsing and binding in `tsc` 5.9. `complex-types` defaults to 1000 types, past which `tsc` check time and memory grow visibly. `large-literals` defaults to 10000 values: in `tsc` 5.9, about 0.06s of checking for uniform data and 0.4s for objects of mixed shapes, in every program that loads them.

`cycles` reports every cycle, so its threshold (the number of files in a cycle) defaults to 1. `duplicates` reports every export with a second name, so its threshold (the number of names) defaults to 2. `unused-exports`, `unused-types`, `unused-enum-members`, `unused-namespace-members`, `dependencies` and `catalog` report every unused export, member, package or catalog entry and have no threshold.

Every command exits with status 1 when a finding of any of its rules with `error` severity crosses its threshold, so they can gate CI. Findings with `warn` severity are reported but don't fail the run. Files that fail to parse are listed at the end of the report.

A rule that can't run doesn't stop the others. In a pnpm workspace with a
catalog but no source files, for instance, `dependencies` and
`heavy-dependencies` find no entry files, while `catalog` and
`duplicate-versions` still report. Each rule that failed gets a line such as
`✗ dependencies couldn't run: no entry files found under ...`, and the run
exits with status 1. Turn the rule off in `.oxicleanrc.json` if it doesn't
apply to the project.

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

**Usage** (`barrels` runs with `bloat`):
```bash
oxiclean bloat --root ./my-project
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

**Usage** (both run with `unused`):
```bash
oxiclean unused --root ./my-project
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

**Usage** (both run with `unused`):
```bash
oxiclean unused --root ./my-project
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

Findings are warnings unless the config file sets the rule to `error`.

**Usage** (runs with `dependencies`):
```bash
oxiclean dependencies --root ./my-project
```

**Example Output:**
```
⚠ Heavy dependencies detected (threshold: 100 modules)

package.json
└──  package 'date-fns', imported by 12 files (pulls in 302 modules) [warn]
```

### Duplicate Package Versions Checker

Finds the packages `package-lock.json` (v2 and v3) or `pnpm-lock.yaml` (v6
and v9) installs at more than one version, at or above `threshold`
versions (default 2). Each finding lists the versions, lowest first, and
points at the package's first entry in the lockfile. pnpm's peer variants
of one version count once.

**Usage** (runs with `dependencies`):
```bash
oxiclean dependencies --root ./my-project
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

**Usage** (runs with `dependencies`):
```bash
oxiclean dependencies --root ./my-project
```

### Program Files Checker

This and the checkers below run with `oxiclean typecheck`.

`tsc` parses, binds and type-checks every file in a program. `program-files`
reads every `tsconfig*.json` and `jsconfig*.json` as `tsc` does (following
`extends`, `${configDir}` and its `include`, `exclude` and `files` rules)
and reports files a program checks that nobody needs checked:

- **Build output**: files in any tsconfig's `outDir` or `declarationDir`.
  `tsc` only leaves out a program's own `outDir`, and only when it sets no
  `exclude`, so `dist` from a `tsconfig.build.json` is often checked again
  by `tsconfig.json`, or by the build config itself once it inherits an
  `exclude`.
- **Minified or very large JavaScript** (1 MiB or more) that `allowJs`
  pulls in, such as vendored bundles.

A config counts as a program when it is a `tsconfig.json` or
`jsconfig.json`, another config references it, a package.json script
names it (`tsc -p tsconfig.build.json`), or nothing extends it. `--fix`
adds the files to `exclude`, keeping what the config excluded before.

```
⚠ tsconfig programs check files they don't need

packages/app/tsconfig.json
└──  line 2: type-checks the build output in dist, the output directory of tsconfig.build.json, as if it were source (12 files)
        packages/app/tsconfig.build.json:4: emits to dist
        packages/app/dist/main.d.ts: build output
        fixable with --fix: add "dist" to "exclude"
```

### Overlap Checker

`overlap` reports files that more than one tsconfig program type-checks.
Each program checks its files on its own, so such files are checked once
per program, by `tsc -b`, in the editor and by typed linting. Typical
causes are a root tsconfig whose `include` reaches into packages with their
own tsconfig, and a catch-all `tsconfig.eslint.json`. Configs where one
extends the other, and configs in the same directory, are variants of one
project and aren't compared. Findings go on the config that reaches into
the other's directory.

```
⚠ Files type-checked by more than one tsconfig

tsconfig.eslint.json
└──  line 3: type-checks files that packages/ui/tsconfig.json also type-checks, so they are checked more than once (48 files)
```

### References Checker

With project references, `tsc -b` checks each package once and the packages
that import it read the `.d.ts` it emits. Without them, or with them out of
date, every program that imports a sibling package's source checks that
source again. `references` compares each program's references with the
workspace packages its files import (type-only imports included):

| Code | Meaning | `--fix` |
|---|---|---|
| `missing-reference` | A program that builds with references imports a workspace package it doesn't reference | Adds the reference |
| `stale-reference` | It references a workspace package none of its files import | Removes it |
| `not-composite` | A referenced config isn't `composite`, which `tsc -b` rejects | Sets `composite` |
| `missing-target` | A reference points at a config that doesn't exist | |
| `reference-cycle` | Configs reference each other, which `tsc -b` can't order | |
| `source-imports` | In a repo without references, two or more packages type-check another package's source, through `paths` or a package.json `types` or `exports` pointing at `.ts` | |

Like `tsc`, it follows local imports from each program's root files (so
`.vue` files and files a `files` list leaves out count), counts the
packages `compilerOptions.types` loads, and treats a package as reached
through the configs a reference references in turn. An import reaches a
package's source when `paths`, or else the package's `exports` (with the
program's conditions), `types`, `main` or `index`, lead to a `.ts` file;
`module` is never read. A program builds with references when it has
some, something references it, or it is `composite`. Missing and stale references are only reported
in repos that use references; repos that don't get `source-imports`
instead.

```
⚠ Project references don't match imports

packages/app/tsconfig.json
└──  line 4: imports @acme/ui but doesn't reference its tsconfig, so `tsc -b` checks it again instead of reading its .d.ts (1 config)
        packages/app/src/main.ts:1: imports @acme/ui
        fixable with --fix: add { "path": "../ui" } to "references"
```

### tsconfig Options Checker

`tsconfig` reports compiler options that make every `tsc` run slower:

| Code | Meaning | `--fix` |
|---|---|---|
| `skip-lib-check` | Programs leave `skipLibCheck` unset, so `tsc` checks every `.d.ts` in `node_modules` each run. Reported once on the config they extend; programs made mostly of `.d.ts` files are there to check them and aren't reported | Sets it on that config |
| `incremental` | A package.json script only checks a config with `tsc` (`--noEmit` or `noEmit`, not `-b` or `--incremental`) and the config doesn't set `incremental` or `composite`, so each run starts over. Configs that emit aren't reported, as a `.tsbuildinfo` outside `outDir` makes `rm -rf dist && tsc` emit nothing | Sets `incremental`, with `tsBuildInfoFile` in `node_modules/.tmp` |
| `debug-option` | A config turns on `diagnostics`, `extendedDiagnostics`, `generateTrace`, `listFiles`, `listEmittedFiles`, `explainFiles` or `traceResolution` | Turns it off (not `generateTrace`) |

Configs named for debugging (`tsconfig.explain.json`, say) keep their
debug options. Programs whose `extends` chain names a config that isn't
installed or can't be read are skipped, since it may set these options.

### Ambient Types Checker

Before TypeScript 6, a program without `compilerOptions.types` loads
every package in each `node_modules/@types` from its directory up, used or
not; from 6 on, only with `"types": ["*"]`. In a monorepo that includes
whatever other packages install and the package manager hoists.
`ambient-types` reports programs that load `@types` packages that no
package.json from their directory up declares (nor a deno.json `npm:`
import, nor a declared `@types` package's dependencies), and suggests a
`types` list. It offers no fix: a program may rely on globals only a
sibling package declares. Imports of a package still find its types
without it being listed.

```
⚠ Programs load @types packages they don't declare

packages/app/tsconfig.json
└──  line 3: loads every @types package it can see, including @types/jest that no package.json here declares; list the ones it uses in compilerOptions.types, such as ["node", "react"] (1 package)
        node_modules/@types/jest/index.d.ts: not declared
```

### Isolated Declarations Checker

With `isolatedDeclarations`, every export carries the types its `.d.ts`
needs, so `tsc`, oxc or `tsgo` can write declaration files a file at a
time without checking the program, and `tsc -b` can start dependents
before their dependencies finish. `isolated-declarations` runs oxc's
isolated declarations on each program that emits declarations:

| Code | Meaning | `--fix` |
|---|---|---|
| `can-enable` | Every export already has the types it needs | Turns `isolatedDeclarations` on, which keeps it that way |
| `few-annotations` | Up to 10 exports need an explicit type; each is listed | |
| `slow-types` | A Deno package with `exports` (a JSR package) has exports without explicit types in its public API (its entries and what they re-export), which JSR calls slow types | |

Besides oxc's checks, an exported variable initialized with `satisfies`
needs a type, as `tsc` requires. Programs with `allowJs` (which `tsc`
won't combine with the option), on TypeScript before 5.5, or with a file
that doesn't parse aren't reported. Each program's files are followed
through their local imports, as `tsc` does. The rule has no threshold.

```
⚠ Packages could emit declarations without type-checking

packages/near/tsconfig.json
└──  line 2: emits declarations and is 1 annotation away from isolatedDeclarations, which lets tools write .d.ts files without type-checking (isolatedDeclarations)
        packages/near/src/index.ts:3: TS9010: Variable must have an explicit type annotation with --isolatedDeclarations.
```

### Large Literals Checker

`tsc` builds a type for every value in an object or array literal, and for
every value in each `.json` file a program imports, in every program that
loads them, and again in the editor. `large-literals` counts the values
(array elements and object properties, through nested literals) and
reports:

| Code | Meaning |
|---|---|
| `json` | A `.json` file programs import. A `<name>.d.json.ts` next to it declaring its type, with `allowArbitraryExtensions`, makes `tsc` read that instead (when `tsc` emits JavaScript, it then stops copying the `.json` file to the output, so the build has to). For a dependency's file, or under Deno, reading it at runtime does |
| `literal` | An object or array literal of plain data: strings, numbers, booleans, `null` and nested literals. An annotation doesn't help, since `tsc` still checks each value against it; moving the data to a `.json` file with a declaration, or loading it at runtime, does |

Each program's files are followed through their local imports, as `tsc`
does. Programs without `resolveJsonModule` aren't checked for `.json`
imports; it defaults to on when modules resolve the `bundler` or
`nodenext` way. `require` counts in JavaScript files only, as in `tsc`.
Every directory with a `deno.json`, such as Supabase edge functions, is
checked without a tsconfig, since `deno check` always types the `.json`
files it imports. The default threshold is 10000 values.

```
⚠ Large literals tsc infers types for (threshold: 10000 values)

src/i18n/strings.json
└──  line 1: 18210 values that tsc infers types for in every program that imports it (2 programs); a strings.d.json.ts next to it declaring its type, with allowArbitraryExtensions, lets tsc read that instead (18210 values)
        src/i18n/index.ts:1: imports strings.json
```

### Type Bloat Checker

`bloat` counts the modules an import pulls into a bundle. `tsc` follows a
different graph: the `.d.ts` files a package's types entry imports,
re-exports and references, which it parses and binds in full, with
`skipLibCheck` or without. `type-bloat` resolves each package import the
way `tsc` does for its program (`paths`, `node_modules` from the importing
file up, the package before its `@types` package, `exports` with the
`types` condition when module resolution reads it, else `typesVersions`,
`types`, `typings` and `main`), follows its declarations, and reports
imports whose declarations, beyond the `@types` packages the program
loads anyway, run to 100000 lines or more:

| Import | Lines of declarations | `tsc` 5.9 total time |
|---|---|---|
| `import { google } from "googleapis"` | 3,731,740 | 9.9s |
| `import { drive } from "@googleapis/drive"` | 25,013 | 0.5s |
| `import AWS from "aws-sdk"` | 928,586 | 3.9s |
| `import S3 from "aws-sdk/clients/s3"` | 8,224 | 0.4s |

```
⚠ Imports load huge declaration trees (threshold: 100000 lines)

src/drive.ts
└──  line 1: loads 3731740 lines of declarations in 1002 files that tsc parses and binds in every program that imports it; import what you use from a narrower entry, such as a subpath of googleapis or a smaller package (3731740 lines)
```

### Complex Types Checker

Some type shapes make `tsc` build huge numbers of types, so one alias can
cost more check time and memory than the rest of a file.
`complex-types` reads them from the syntax, following type aliases
across files, and reports those it estimates build at least 1000 types:

| Code | Shape | Estimate |
|---|---|---|
| `template-product` | a template literal type whose holes are unions, such as `` `${State}${Prefix}-${Color}-${Shade}` `` | the product of the union sizes |
| `union-intersection` | an intersection of unions, which `tsc` normalizes into a union of every combination | the product of the union sizes; an intersection of literal unions is their common members, and `A & A` is `A` |
| `recursive-paths` | a `Paths<T>`-style alias with no depth limit, recursing in a template literal or through a helper such as `Join<K, Paths<T[K]>>`, used on a `.json` import, an object type or an interface | the key paths it builds |
| `tuple-arithmetic` | a `Range<N>`-style alias that grows a tuple one element at a time | N(N+1)/2 for the largest `N` in each file, whose tuples the others reuse |
| `builder-chain` | a chain of zod, tRPC, Hono, Elysia or Drizzle calls whose methods each rebuild the type built so far, such as zod's `.extend()` but not `.min()`, followed through the local `const`s it starts from | 115·links + links², fitted to zod `.extend()` chains |

These are estimates, not measurements; `tsc --generateTrace` shows what
the types cost. Type parameters hide aliases of the same name, and so do
types declared in namespaces, functions and `declare module` blocks.
`--fix` writes each `Range<N>` in a file out as the union
`0 | 1 | … | N-1`, in parentheses where the union would otherwise bind
differently, when the alias has the usual shape
(`A['length'] extends N ? A[number] : Range<N, [...A, A['length']]>`) and
N is at most 256. In the Bun fixture this cut `tsc`'s type count from
22289 to 8528.

```
⚠ Types that expand to huge numbers of types (threshold: 1000 types)

packages/server/src/config.ts
├──  line 3: `Range<200>` builds 200 tuples, each one element longer than the last (about 20100 types), which the other use in this file reuses; write the union out, or use number and check it at runtime (20100 types)
│       packages/server/src/config.ts:4: `Range<10>` reuses them
│       fixable with --fix: write the 2 uses of Range out as unions
└──  line 5: `Tup<120>` builds 120 tuples, each one element longer than the last (about 7260 types); write the union out, or use number and check it at runtime (7260 types)
```

These checks skip tsconfigs in test input directories (`fixtures`,
`__fixtures__`, `testdata`, `goldens`, `test-output`, `__snapshots__`): they are data
for tests, not programs anyone runs. Deno doesn't read tsconfig files, so
in a Deno workspace only `isolated-declarations` (for JSR slow types) and
`large-literals` find anything, unless it keeps a tsconfig for editors.

### Trace

`oxiclean trace [DIR]` reads the trace that `tsc --generateTrace DIR`
(or `tsc -b --generateTrace DIR`) already wrote; it never runs `tsc`. It
reads `trace.json` and `types.json`, or `legend.json` with one
`trace.N.json` and `types.N.json` per program of a build. `DIR` defaults
to `trace.dir` in `.oxicleanrc.json`, relative to the root:

```jsonc
{
  "trace": { "dir": "trace" },
  "rules": {
    // analyze-trace's --forceMillis and --skipMillis
    "hot-spots": ["warn", { "threshold": 500, "skipMillis": 100 }],
    "duplicate-packages": "warn"
  }
}
```

`hot-spots` is a port of
[`@typescript/analyze-trace`](https://github.com/microsoft/typescript-analyze-trace)'s
hot-spot tree: spans of checking files, checking expressions and
declarations, comparing types, determining variance and emitting
declarations that took at least `threshold` ms, or at least 60% of the
span they're in, with spans shorter than `skipMillis` dropped as the trace
is read. Positions are shown as `(line, char)` like analyze-trace's, and
types are named from `types.json`. A span that was still running when the
trace ended (tsc ran out of memory, or the file was cut short) runs to the
end of the trace. A hot spot inside a type that `complex-types` reports
says so, links to that finding and is at least as severe as it (an
`error` by default): the estimate and the measurement agree. Each hot
spot is held to the threshold of the file it's reported on, so
`overrides` can raise or lower it per package or path; a span under its
threshold is still reported when it takes most of a span that is.
`skipMillis` applies to the whole trace, so it is read only from
`rules`.

`duplicate-packages` reports packages `tsc` loaded from more than one
directory, under `node_modules` in npm, Yarn, Bun and pnpm layouts and
Deno's `node_modules/.deno`, with each copy's version and whether
deduping or aligning versions would fix it. analyze-trace reads only
`findSourceFile` events, which recent `tsc` versions often leave out for
`node_modules`, so oxiclean also reads `createSourceFile`.

Paths in the trace may come from another machine (a CI runner, or
Windows), a symlinked checkout or a lowercased path: oxiclean learns the
directory the trace was recorded in, from a `tsc --build` legend's
tsconfig files or else the files the trace checked, and finds every path
under the root through it, so an unrelated `src/index.ts` elsewhere is
never taken for the root's. Spans in files outside the root are reported
on the nearest enclosing span in a file inside it, else on the trace file
or the root's package.json; oxiclean reads only regular files under the
root. Copies of a package that are one directory through symlinks are
one copy.

```
⚠ Hot spots in the trace (threshold: 10 ms)

packages/server/src/config.ts
├──  Check file packages/server/src/config.ts took 309ms (309ms) [warn]
└──  line 3: Check variable declaration from (line 3, char 14) to (line 3, char 35) took 277ms; this confirms the complex-types finding at packages/server/src/config.ts:3 (tuple-arithmetic, about 80200 types) (277ms)
        packages/server/src/config.ts: inside Check file packages/server/src/config.ts
        packages/server/src/config.ts:3: complex-types: `Range<400>` builds 400 tuples, each one element longer than the last (about 80200 types), which the other use in this file reuses; write the union out, or use number and check it at runtime

⚠ Packages tsc loads more than once (threshold: 2 directories)

node_modules/@acme/shared/package.json
└──  tsc loads `@acme/shared` from 2 directories: 1.0.0 at node_modules/@acme/shared, 1.0.0 at packages/server/node_modules/@acme/shared; the same version installed twice; dedupe the lockfile, or hoist the package (2 directories) [warn]
```

`scripts/analyze-trace-parity.sh` compares the two tools on fresh traces
of the fixtures and of a real repo; it runs from the manually triggered
`analyze-trace parity` workflow. In CI, a test compares every hot spot of
the committed fixture traces with analyze-trace's checked-in output.

### Lint Config Checker

`oxiclean lint` runs `lint-config`, which reads ESLint 8 configs
(`.eslintrc.*` and package.json `eslintConfig`, with `overrides` and local
`extends`), ESLint 9 flat configs (`eslint.config.{js,mjs,cjs,ts,mts,cts}`)
and oxlint configs (`.oxlintrc.json(c)` and `oxlint.config.(m)ts`, and any
other a script passes to `--config`) with oxc, without running ESLint,
oxlint or Node, and reports settings that make linting slower than it
needs to be. It defaults to `warn`. Like ESLint, it reads one eslintrc per
directory and none where a flat config is in or above it, and it looks up
nested flat configs per file only for ESLint 10 (or 9 with the
`v10_config_lookup_from_file` flag); ESLint 9 uses the config of the
directory it runs in.

| Code | Reported when | `--fix` |
|---|---|---|
| `typed-js-files` | typescript-eslint's parser and type-aware parsing (`parserOptions.project` or `projectService`) apply to JavaScript files ESLint lints too, such as `eslint.config.js`, so typescript-eslint type-checks them, or fails to parse those no tsconfig includes | |
| `project-globs` | `parserOptions.project` names more than one tsconfig, through a list or a glob, so typescript-eslint builds a program for each | |
| `project-service` | `project: true` where the installed typescript-eslint is 8 or later, whose `projectService` shares one service across files | |
| `tsconfig-root-dir` | relative `project` paths without `tsconfigRootDir`, which resolve from wherever ESLint runs (typescript-eslint 8.37 and later infer it for flat configs that read `tseslint.configs`) | |
| `slow-rule` | `import/no-cycle`, `import/no-unused-modules`, `import/namespace` or `import/no-deprecated` (or the `import-x/` ones) is on, each with what does its job faster, such as `oxiclean cycles` or oxlint's `import/no-cycle` | |
| `eslint-plugin-oxlint` | a package runs oxlint too (a script, a dependency or an oxlint config) and ESLint doesn't use eslint-plugin-oxlint to turn off the rules oxlint already checks | |
| `build-output` | build output the linter doesn't ignore: framework output such as `.next` and `.svelte-kit`, tsconfig `outDir`s, and `dist`, `build`, `out` and `coverage` when git ignores them. ESLint reads `.eslintignore` and `--ignore-pattern` in scripts; oxlint skips what `.gitignore` ignores, so only committed output is reported for it | Adds the directories to `ignorePatterns` of JSON eslintrc and oxlint configs, and to the global ignores of flat configs whose exported array is written out in the file |
| `type-aware-unused` | oxlint's type-aware mode is on, in `options.typeAware` or `--type-aware` in a script, but every type-aware rule is off | Turns `options.typeAware` off in the config |

Configs are evaluated statically: imports of local files and workspace
packages are followed, spreads, `tseslint.config()`, `defineConfig()`,
FlatCompat and `globalIgnores()` are understood, and an entry behind a
condition counts as maybe applying. A config built in ways it can't
follow, such as from a shared config package or an array changed after
it's declared, makes the checks that depend on it stay quiet rather than
guess. So do YAML eslintrc files, which aren't read, and a
`.eslintrc.json` with a trailing comma, which ESLint 8 can't read either.

```
⚠ Lint configs that slow linting down

.eslintrc.json
├──  line 10: turns on import/namespace, which parses every module imported with `import * as` to check the names used on it; TypeScript checks the same in .ts files, and oxlint's import/namespace runs in the same pass as its other rules (1 item)
├──  line 7: doesn't ignore build output, so ESLint parses generated files in `packages/lib/build` (2 files); add it to its ignores (1 item)
│       packages/lib/tsconfig.json:3: emits build output there
│       fixable with --fix: add "packages/lib/build/**" to "ignorePatterns"
└──  line 5: turns on type-aware parsing for 1 JavaScript file too, such as `packages/lib/src/legacy.js`, so typescript-eslint type-checks them along with the TypeScript files, or fails to parse those no tsconfig includes; set it in an entry with `files` for TypeScript only, or add typescript-eslint's disableTypeChecked for JavaScript files (1 item)
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
    "cycles": ["warn", { "threshold": 3 }],
    // `oxiclean unused` skips this rule; the other three still run
    "unused-types": "off",
    // Fail the run on heavy packages instead of warning
    "heavy-dependencies": ["error", { "threshold": 200 }]
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
A rule listed with only a severity keeps its default threshold. A rule not
listed is on, at `error`, or at `warn` for `heavy-dependencies`.

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

## Fixes

With `--fix`, a command applies the fixes the findings of all its rules
offer and reports only what is left. One fix can make another possible, as
`references` making a program `composite` lets `isolated-declarations`
turn on `isolatedDeclarations`, so the command runs again after each round
of fixes until no new fix applies (at most 5 rounds), and reports how many
findings it fixed in all. A fix is offered only when it needs no decision, such
as adding build output to a tsconfig's `exclude`; findings that have one
say `fixable with --fix` in the text report and carry a `fix` in JSON.
Fixes to JSON files keep their comments and formatting, and fixes to
JavaScript and TypeScript configs only insert text.

## Program map

`oxiclean map` opens an interactive view of the type checking a workspace
does. A file should be checked once, by the tsconfig of the package it is
in. The map counts every other program that checks it as a repeat, and
shows:

- the share of all file checks that are repeats;
- the causes of the repeats: imports that reach another package's source
  without a project reference, lint tsconfigs that include everything,
  test tsconfigs that include source too, build output checked as source,
  and other `include` patterns that reach into other packages;
- the packages with the most repeats, ranked (packages checked once are
  counted, not listed, so the list stays short in large monorepos);
- for the selected package, each program that checks it, grouped by cause,
  with the import or file that brings it in, and the usual fix.

Keys: `↑`/`↓` or `j`/`k` move, `1`–`5` show only one cause, `0` shows all,
`/` searches package names, `r` scans again, `q` quits. The scan runs while
the view shows its progress. Set `NO_COLOR` for no color and
`OXICLEAN_NO_MOTION` to turn off animation. `--format json` prints the same
map as JSON without opening the view. For a source import the map counts
every file the package's own tsconfig checks, an upper bound, as `tsc` only
checks the files the imports reach.

## Time saved and health score

Not every finding costs the same. A tsconfig that type-checks 2,000 files
twice slows every `tsc` run, every editor reload and every CI pipeline; an
unused export costs a fraction of a millisecond. So every finding comes with
an estimate of the time fixing it saves, and every report ranks findings by
it and turns the total into a health score:

```
Health: 72/100 (C)
  Fixing these findings saves about 6.1h of developer time and 2.4h of CI time a week, for 10 developers and 30 CI runs a day (`savings` in .oxicleanrc.json).
  Biggest wins:
    1. 2.8h dev, 51m CI a week  overlap  tsconfig.eslint.json:3
       type-checks files that packages/web/tsconfig.json also type-checks, so they are checked more than once
    2. 1.4h dev, 25m CI a week  references  packages/ui/tsconfig.json
       @acme/ui's source is type-checked again by each of 3 packages that import it
    ...
```

Every command ends its text report with the score and its five biggest
wins. `oxiclean health` runs every command, and prints the score, the 15
biggest wins and what each rule's findings cost, without listing every
finding; run a rule's command for those. It exits with status 1 like the
commands it runs, and doesn't take `--fix`: run that with the command whose
findings to fix.

### How the time is estimated

Each finding is turned into the seconds it adds to one full run of each tool
it slows: `tsc` (and the editor loading the project), the linter, the test
runner, the dev server or bundler, and the package manager. Those seconds
become hours a week from how often the team runs each tool:

- **Developer time**: seconds per run × runs a developer starts each day ×
  developers × working days. Developer time is waiting time.
- **CI time**: seconds per run (all tools added up) × CI runs a day ×
  working days. Findings that only cost time locally, such as a missing
  `incremental`, cost no CI time.

Findings are ranked by developer hours plus CI hours divided by
`ciHoursPerDeveloperHour` (4 by default: CI compute is cheap, but people
wait on part of it). The health score compares what the findings cost
with what the same project's tools would take without them, estimated
from its number of source files: `100 × baseline / (baseline + findings)`.
A project whose findings cost as much time again as the project itself
scores 50. Grades are A from 90, B from 80, C from 70, D from 60, F below.
A command's score only counts the rules it ran; `oxiclean health` counts
them all.

The per-run costs are typical numbers, not measurements of your project,
so treat the hours as a way to compare findings, not a forecast. Where a
fix's effect is uncertain the estimate is conservative: a finding about how
many modules an import reaches saves only what brings it back to its
threshold. Several findings about the same thing count once, with the
largest standing for the rest: an entry's imports are part of its whole
graph, and a bundler transforms a module once however many entries reach
it, so `bloat` and `depth` count one entry per workspace package; tsc reads
a package's declarations once a program (`type-bloat`); a package is
installed once however many manifests list it (`heavy-dependencies`); and
`lint-config` prices each kind of issue on the whole project's lint time
once, however many configs have it. A span of a trace is part of the span
it's in (`hot-spots`). No finding costs one tool more than 120 seconds a
run. Findings are listed as long as they cost time on some run, even when a
team setting makes their hours zero.

The unit costs, for one CI runner core, cold:

| Cost | Seconds | Basis |
|---|---|---|
| `tsc` checking an average source file | 0.010 | tsc 5.9 checks 1,000 average files in about 10 s |
| Linting a file without type information | 0.006 | about 60% of `tsc`'s time per file |
| A test runner loading a module | 0.002 | transform and evaluate |
| A bundler or dev server transforming a module | 0.001 | |
| A dev server waiting for one more level of unbundled imports | 0.005 | one round trip |
| Installing a package, warm cache | 0.25 | |
| Installing a healthy project's dependencies | 15 | baseline only |
| `tsc` parsing and binding a line of declarations | 0.0000025 | 100,000 lines take about 0.25 s |
| `tsc` inferring one value of a literal | 0.00002 | 10,000 values take 0.06 to 0.4 s |
| `tsc` creating one type a complex type expands to | 0.00005 | |
| `tsc` checking `node_modules` declarations for one program | 1.0 | `skipLibCheck` off |
| `tsc` loading an `@types` package | 0.05 | |
| `tsc` loading a package's declarations a second time | 0.2 | |

How each rule's findings use them:

| Rule | Saves each run |
|---|---|
| `bloat` | modules over the threshold × (test + build cost per module), for the largest entry of each package |
| `barrels`, `heavy-dependencies` | modules over the threshold × test cost × a quarter of the importing files (test runners load a module again for each test file), plus the build cost once |
| `depth` | levels over the threshold × a dev server round trip, for the deepest import of each package, locally only |
| `cycles` | files in the cycle × (test + build cost per module), locally only: editing any of them re-runs all |
| `unused-*`, `duplicates` | a twentieth of an average file's `tsc` and lint time per unused item or extra name |
| `dependencies`, `duplicate-versions` | one package install per unused package or extra version |
| `catalog`, and `references` findings that stop `tsc -b` | nothing: tidier, or broken rather than slow |
| `program-files`, `overlap` | `tsc` time of each file it shouldn't check (minified or very large JavaScript counts as ten files) |
| `references` | half the `tsc` time of an average package for each package that checks another's source; a tenth for a stale reference |
| `tsconfig` | `skipLibCheck`: 1 s per program; `incremental`: half a package's `tsc` time, locally only; debug options: 5% to 30% of a package's `tsc` time |
| `ambient-types` | 0.05 s per `@types` package a program loads without declaring it |
| `isolated-declarations` | a fifth of a package's `tsc` time: declarations emitted without type-checking, packages built in parallel |
| `large-literals` | `tsc` time per value |
| `type-bloat` | `tsc` time per declaration line over the threshold |
| `complex-types` | `tsc` time per type |
| `hot-spots` | 80% of the measured time of each outermost span |
| `duplicate-packages` | 0.2 s per extra copy |
| `lint-config` | a share of the project's lint time: typed linting through `project` globs 30% of a `tsc` run, an unused type-aware setup 50%, type-aware parsing of JavaScript 10%; `import/no-cycle` and `import/no-unused-modules` 50% of untyped lint time, other slow rules 15%, a missing `eslint-plugin-oxlint` 20%, each unignored build output directory 10% |

### Describing your team

The defaults describe a team of 10 developers. Set `savings` in
`.oxicleanrc.json` to match yours; `developers`, `workDaysPerWeek` (1 to 7)
and `ciHoursPerDeveloperHour` are at least 1:

```jsonc
{
  "savings": {
    "developers": 10,
    "workDaysPerWeek": 5,
    // CI pipeline runs a working day, across the team
    "ciRunsPerDay": 30,
    // CI hours that weigh as much as one hour of a developer waiting
    "ciHoursPerDeveloperHour": 4,
    // Full runs of each tool one developer starts a working day
    "localRunsPerDay": { "typecheck": 10, "lint": 5, "test": 10, "build": 5, "install": 1 }
  }
}
```

In `--format json`, each finding carries a `savings` object
(`seconds_per_run` for each tool it slows, `scope`, and
`developer_hours_per_week`, `ci_hours_per_week` and
`weighted_hours_per_week`), or `null` when its time is counted with another
finding. The document's `savings` holds the `health` score and grade, the
totals, the `baseline`, each rule's share in `by_rule`, the ten biggest wins
in `top`, and the `assumptions` they were computed with. SARIF results carry
the same estimate in `properties.savings`, and the run carries the summary
in `properties.savings`.

## Output formats

`--format text` (the default) prints a section for each rule with
findings, as shown above, then a line for each rule without, then the
health score and biggest wins (see [Time saved and health
score](#time-saved-and-health-score)), then one summary for the command:

```
⚠ Unused exports detected

src/button.ts
└──  export 'unusedHelper' (never imported)

────────────────────────────────────────────────────────────
Summary
  Total violations: 1
  Top 1 offenders:
    1. export 'unusedHelper' (never imported) - src/button.ts

✓ No unused exported types detected.
✓ No unused enum members detected.
✓ No unused namespace members detected.

Health: 100/100 (A)
  Fixing these findings saves about <1s of developer time and <1s of CI time a week, for 10 developers and 30 CI runs a day (`savings` in .oxicleanrc.json).
  Biggest wins:
    1. <1s dev, <1s CI a week  unused-exports  src/button.ts:2
       export 'unusedHelper'

● Finished in 19ms on 2 files (using 8 threads).
● Ran 4 rules (unused-exports, unused-types, unused-enum-members, unused-namespace-members): 1 error, 0 warnings.
● Resolved 1 of 1 imports (100.0%), 0 built-ins skipped.
```

`--format json` prints one JSON document with the `command`, an entry in
`rules` for each rule that is on (its `severity` and `threshold` outside any
override, its counts of findings, errors and warnings, and an `error` that
says why it couldn't run, or `null`), a `summary`
(files analysed, error and warning counts, import resolution) and every
finding with its `rule`, `severity`, `file` (relative to the root, with
`/`), `line`, `package`, `request`, `resolved`, `value`, `threshold` and a
`message`. A rule that is off outside the overrides and on in some has
`"severity": "off"`; its findings carry the severity of their override. A
rule that couldn't run has `null` for `severity` and `threshold`.

`--format sarif` prints a [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
log, which GitHub code scanning and other tools can show inline on pull requests:

```yaml
- run: oxiclean unused --format sarif > oxiclean.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: oxiclean.sarif
```

A command's log has one run that lists every rule that is on, and each
result names its rule. A rule that couldn't run is an `error`
notification in the run's `invocations[0].toolExecutionNotifications`,
pointing at the rule, and makes the invocation's `executionSuccessful`
false. Each command has its own code-scanning category
(`oxiclean/<command>/`), so uploading several commands for one commit keeps
every command's alerts. Results carry `partialFingerprints` that ignore line
numbers, so an alert stays the same alert when code above it moves.

## Migrating from per-rule commands

Each rule used to be a command of its own. Now each command runs a group
of rules, and `.oxicleanrc.json` turns rules off:

| Before | Now |
|---|---|
| `oxiclean unused-exports`, `unused-types`, `unused-enum-members`, `unused-namespace-members` | `oxiclean unused` |
| `oxiclean dependencies`, `heavy-dependencies`, `catalog`, `duplicate-versions` | `oxiclean dependencies` |
| `oxiclean program-files`, `overlap`, `references`, `tsconfig`, `ambient-types`, `isolated-declarations`, `large-literals`, `type-bloat` | `oxiclean typecheck` |
| `oxiclean bloat`, `barrels` | `oxiclean bloat` |
| `oxiclean depth`, `cycles`, `duplicates` | unchanged |

To keep running one rule alone, as `oxiclean unused-types` did, run its
command with the command's other rules off:

```json
{
  "rules": {
    "unused-exports": "off",
    "unused-enum-members": "off",
    "unused-namespace-members": "off"
  }
}
```

Other changes to look out for:

- `oxiclean bloat` now also runs `barrels`, and `oxiclean dependencies` now
  also runs `heavy-dependencies`, `catalog` and `duplicate-versions`. A CI
  job that ran `bloat` or `dependencies` may find more; turn the new rules
  off to keep it as it was.
- `heavy-dependencies` now defaults to `warn`, so its findings no longer
  fail the run. Set it to `"error"` to keep them failing.
- JSON output names the `command` at the top level and lists each rule in
  `rules[]`, with its own `severity`, `threshold` and counts, instead of
  the top-level `rule` and `threshold`. Each finding names its `rule`.
- SARIF's `automationDetails.id`, the code-scanning category, is now
  `oxiclean/<command>/` rather than `oxiclean/<rule>/`. Alerts uploaded
  under the old categories aren't replaced by the new ones and stay open:
  once the new uploads run, remove the old configurations on the
  code-scanning tool status page (or delete their analyses through the
  API), or dismiss their alerts.

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
`workspaces` field (npm, yarn and bun), the root `deno.json` `workspace`
field (Deno, whose packages may name themselves in `deno.json`) or, failing
those, `lerna.json` resolve
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
