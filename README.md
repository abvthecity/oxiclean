# Oxiclean 🧼

A Rust monorepo for code quality and analysis tools for JavaScript/TypeScript projects.

## Quick Start

```bash
# Install
cargo install --path crates/oxiclean

# Run import bloat checker
oxiclean import-bloat --root ./my-project --threshold 300

# Run import depth checker
oxiclean import-depth --root ./my-project --threshold 20

# Get help
oxiclean --help
oxiclean import-bloat --help
oxiclean import-depth --help
```

## Development

```bash
cargo build                                       # build
cargo run -- import-bloat --root ./my-project     # run the CLI from source
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for tests, linting and benchmarks.

## Project Structure

```
oxiclean/
├── crates/
│   ├── oxiclean/              # Main CLI application (binary)
│   ├── oxiclean_core/         # Shared utilities (parser, resolver, etc.)
│   ├── oxiclean_import_bloat/ # Import bloat detection library
│   └── oxiclean_import_depth/ # Import depth analysis library
└── Cargo.toml                 # Workspace configuration
```

## Tools

Both tools accept the same options for choosing what to analyse:

| Option | Meaning |
|---|---|
| `--root <DIR>` | Project root. Defaults to the git root of the current directory. |
| `--entry-glob <GLOB>` | Only treat files matching this glob (relative to the root) as entries, e.g. `"src/pages/**/*.tsx"`. `*` stays within one directory; `**` crosses directories. A pattern without glob characters matches any path containing it. Repeat to give several. Without it, every JS/TS file inside a `src` directory is an entry. |
| `--include-dynamic` | Also follow dynamic `import()` calls. They are skipped by default because lazily loaded modules are split into separate chunks. |
| `--threshold <N>` | Flag findings at or above N (default: 300 modules for `import-bloat`, 20 levels for `import-depth`). Replaces every threshold in the config file. |
| `--package <NAME>` | Only analyse entries in this workspace package. Globs such as `"@scope/*"` work. Repeat to give several. |
| `--config <FILE>` | Read this config file instead of `oxiclean.toml` in the root. |
| `--format <text\|json\|sarif>` | Output format (see [Output formats](#output-formats)). |

The defaults flag roughly the worst 5% of files in Next.js and the worst 10–25% in Expo (see [bench/README.md](bench/README.md#choosing-thresholds)); use `oxiclean.toml` to tune them per package.

Both exit with status 1 when a finding with `error` severity crosses its threshold, so they can gate CI. Findings with `warn` severity are reported but don't fail the run. Files that fail to parse are listed at the end of the report.

### Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes. Counts the total number of unique modules reachable from import statements (breadth-first analysis).

**Usage:**
```bash
oxiclean import-bloat --root ./my-project --threshold 300
oxiclean import-bloat --entry-glob "src/**/*.tsx" --threshold 100
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
- Skips dynamic `import()` unless `--include-dynamic` is given
- Respects `.gitignore` patterns, even outside a git repository
- Skips test files, `node_modules` and `.d.ts` declarations

### Import Depth Checker

Detects excessive import depth in JavaScript/TypeScript projects that can lead to slow module resolution and complex dependency chains. Measures the longest chain of imports from an entry point (depth-first analysis).

**Usage:**
```bash
oxiclean import-depth --root ./my-project --threshold 20
oxiclean import-depth --entry-glob "src/**/*.tsx" --threshold 15
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
  - `import { type Foo }` - Counted (runtime with type specifier)
- Resolves Node.js modules and TypeScript path mappings
- Handles very deep chains without recursion

### Difference Between Tools

- **Import Bloat**: Counts total reachable modules (breadth) - indicates bundle size impact
- **Import Depth**: Counts longest import chain (depth) - indicates module resolution complexity

Both metrics provide complementary insights into code complexity.

## Configuration

Put an `oxiclean.toml` in the project root to configure rules without
repeating flags. Every key is optional.

```toml
# Files that are never entries and never reported, relative to the root
ignore = ["**/fixtures/**", "apps/playground/**"]
# Used when --entry-glob isn't given
entry-globs = ["packages/*/src/**/*.{ts,tsx}"]
include-dynamic = false

[rules.import-bloat]
threshold = 300
severity = "error"         # "error" fails the run; "warn" only reports
ignore = ["scripts/**"]    # files this rule doesn't report on

[rules.import-depth]
enabled = false

# Settings for some workspace packages or paths. Overrides apply in order, so
# later ones win. `packages` matches package names (globs allowed) and `files`
# matches paths relative to the root; when both are given, a file must match both.
[[overrides]]
packages = ["@acme/legacy-*"]
[overrides.rules.import-bloat]
threshold = 1000
severity = "warn"

[[overrides]]
packages = ["@acme/web"]
[overrides.rules.import-depth]
enabled = true
threshold = 12
```

Unknown keys and rule names are errors, so a typo can't silently do nothing.
Flags given on the command line win over the file: `--entry-glob` replaces
`entry-globs`, and `--threshold` replaces every threshold, overrides included.

## Output formats

`--format text` (the default) prints the report shown above.

`--format json` prints one JSON document with a `summary` (files analysed,
error and warning counts, import resolution) and every finding with its
`rule`, `severity`, `file` (relative to the root, with `/`), `line`,
`package`, `request`, `resolved`, `value`, `threshold` and a `message`.

`--format sarif` prints a [SARIF 2.1.0](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
log, which GitHub code scanning and other tools can show inline on pull requests:

```yaml
- run: oxiclean import-bloat --format sarif > oxiclean.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: oxiclean.sarif
```

## Debugging & Logging

Oxiclean logs diagnostics with [`tracing`](https://docs.rs/tracing) to stderr, so stdout carries only the report. Enable logging via the `RUST_LOG` environment variable (errors only by default):

```bash
# Show all debug logs
RUST_LOG=debug oxiclean import-depth

# Show only info level
RUST_LOG=info oxiclean import-bloat

# Show trace level (very verbose)
RUST_LOG=trace oxiclean import-depth

# Filter by module
RUST_LOG=oxiclean_core::resolver=trace oxiclean import-depth
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

Use `--package` to check one package at a time, and `[[overrides]]` in
`oxiclean.toml` to give packages their own thresholds. JSON and SARIF output
name the package each finding belongs to.

## What counts as an import

Static `import`, re-exports (`export * from`, `export { x } from`), `require()`
calls anywhere in a file, and TypeScript `import x = require()` are followed.
Type-only imports and exports are ignored. Dynamic `import()` is not followed,
because it loads the module as a separate chunk, which is the usual fix for
bloat. Node.js built-ins such as `fs` or `node:path` are skipped.

## License

MIT
