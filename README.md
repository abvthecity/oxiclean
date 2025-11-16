# Oxiclean 🧼

A Rust monorepo for code quality and analysis tools for JavaScript/TypeScript projects.

## Quick Start

```bash
# Install
cargo install --path apps/oxiclean

# Run import bloat checker
oxiclean import-bloat --root ./my-project --threshold 200

# Run import depth checker
oxiclean import-depth --root ./my-project --threshold 10

# Get help
oxiclean --help
oxiclean import-bloat --help
oxiclean import-depth --help
```

## Development

```bash
# Build
just build

# Run tools (from monorepo)
just run-oxiclean import-bloat --root ./project
just run-oxiclean import-depth --root ./project

# Quality checks
just test      # Run tests
just lint      # Run clippy
just fmt       # Format code
just ci        # Run all checks

# Show all commands
just
```

## Project Structure

```
oxiclean/
├── apps/
│   └── oxiclean/              # Main CLI application
├── crates/
│   ├── oxiclean_core/         # Shared utilities (parser, resolver, etc.)
│   ├── oxiclean_import_bloat/ # Import bloat detection library
│   └── oxiclean_import_depth/ # Import depth analysis library
├── Cargo.toml                 # Workspace configuration
└── justfile                   # Build commands
```

## Tools

### Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes. Counts the total number of unique modules reachable from import statements (breadth-first analysis).

**Usage:**
```bash
oxiclean import-bloat --root ./my-project --threshold 200
oxiclean import-bloat --entry-glob "src/**/*.tsx" --threshold 100
```

**Example Output:**
```
⚠ Import bloat detected (threshold: 200 modules)

src/pages/Dashboard.tsx (345 modules)
├──  import './components/DataTable' (234 modules)
└──  import '@/utils/api' (156 modules)
```

**Features:**
- Resolves Node.js modules and TypeScript path mappings
- Handles static and dynamic imports
- Respects `.gitignore` patterns
- Skips test files

### Import Depth Checker

Detects excessive import depth in JavaScript/TypeScript projects that can lead to slow module resolution and complex dependency chains. Measures the longest chain of imports from an entry point (depth-first analysis).

**Usage:**
```bash
oxiclean import-depth --root ./my-project --threshold 10
oxiclean import-depth --entry-glob "src/**/*.tsx" --threshold 15
```

**Example Output:**
```
⚠ Excessive import depth detected (threshold: 10 levels)

src/pages/Dashboard.tsx
├──  import './components/DataTable' (depth: 15)
└──  import '@/utils/format' (depth: 12)
```

**Features:**
- Computes maximum depth using DFS with cycle detection
- Correctly handles TypeScript type imports:
  - `import type { Foo }` - Ignored (type-only)
  - `import { type Foo }` - Counted (runtime with type specifier)
- Resolves Node.js modules and TypeScript path mappings
- Handles circular dependencies gracefully

### Difference Between Tools

- **Import Bloat**: Counts total reachable modules (breadth) - indicates bundle size impact
- **Import Depth**: Counts longest import chain (depth) - indicates module resolution complexity

Both metrics provide complementary insights into code complexity.

## Debugging & Logging

Oxiclean uses the `log` crate with `env_logger` for debug output. Enable logging via the `RUST_LOG` environment variable:

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

## License

MIT
