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

# Find circular imports
oxiclean cycles --root ./my-project

# Find modules that export the same thing twice
oxiclean duplicates --root ./my-project

# Get help
oxiclean --help
oxiclean bloat --help
oxiclean depth --help
oxiclean cycles --help
oxiclean duplicates --help
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
│   ├── oxiclean_duplicates/         # Duplicate export detection library
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

The default thresholds are 300 modules for `bloat` and 20 levels for `depth`. They flag roughly the worst 5% of files in Next.js and the worst 10–25% in Expo (see [bench/README.md](bench/README.md#choosing-thresholds)).

`cycles` reports every cycle, so its threshold (the number of files in a cycle) defaults to 1. `duplicates` reports every export with a second name, so its threshold (the number of names) defaults to 2.

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
two names. It checks every parsed file outside `node_modules`.

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

### Difference Between Tools

- **Import Bloat**: Counts total reachable modules (breadth) - indicates bundle size impact
- **Import Depth**: Counts longest import chain (depth) - indicates module resolution complexity
- **Import Cycles**: Lists files that import each other - order-dependent initialisation bugs and code that can't be split
- **Duplicate Exports**: Lists exports with a second name - two ways to import one thing

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
  // an entry. `*` stays within one directory and `**` crosses directories.
  "entries": ["packages/*/src/**/*.{ts,tsx}"],
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
bloat. Node.js built-ins such as `fs` or `node:path` are skipped.

## License

MIT
