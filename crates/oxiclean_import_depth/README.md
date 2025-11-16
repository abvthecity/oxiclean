# Import Depth Checker

Detects excessive import depth in JavaScript/TypeScript projects that can lead to slow module resolution and complex dependency chains.

## Installation

```bash
# Install oxiclean CLI
cargo install --path apps/oxiclean

# Or run from monorepo
just run-import-depth --help
```

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean import-depth

# Common options
oxiclean import-depth --root ./my-project --threshold 10 --entry-glob "src/**/*.tsx"
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `--threshold <N>` - Max import depth before warning (default: 10)
- `--entry-glob <PATTERN>` - Glob pattern to filter entry files (default: all files in `/src/`)

## Example Output

```
⚠ Excessive import depth detected (threshold: 10 levels)

src/pages/Dashboard.tsx
├──  import './components/DataTable' (depth: 15)
└──  import '@/utils/format' (depth: 12)

📊 Statistics:
  Files analyzed: 234
  Warnings found: 8
```

## What is Import Depth?

Import depth measures the number of module traversals required to resolve an import statement. For example:

```typescript
// File: src/index.ts
import { Button } from './components/Button';
// If Button.ts imports from './base', which imports from './theme',
// which imports from './constants', the depth is 4
```

High import depth can indicate:
- Deep dependency chains that are hard to understand
- Slow module resolution and bundling times
- Circular dependency risks
- Overly coupled modules

## Difference from Import Bloat

- **Import Bloat**: Counts the total number of unique modules reachable from an import (breadth)
- **Import Depth**: Counts the longest chain of imports from an entry point (depth)

Both metrics are useful for understanding different aspects of code complexity.

## Type Imports

The tool correctly handles TypeScript type imports:

- `import type { Foo } from 'bar'` - **Ignored** (entire import is type-only)
- `import { type Foo } from 'bar'` - **Counted** (runtime import with type specifier)
- `import { type Foo, Bar } from 'bar'` - **Counted** (has runtime import `Bar`)

This is important because the positioning of the `type` keyword affects whether code is included at runtime, which impacts depth calculations.

## How It Works

1. Parses JavaScript/TypeScript files using the fast OXC parser
2. Resolves all imports (static `import`, dynamic `import()`, and `require()`)
3. Builds a dependency graph and computes maximum depth using DFS
4. Reports files/imports exceeding the threshold

**Features:**
- Resolves Node.js modules and TypeScript path mappings from `tsconfig.json`
- Handles static and dynamic imports
- Respects `.gitignore` patterns
- Skips test files
- Uses memoization for efficient depth computation

## Library Usage

```rust
use oxiclean_import_depth::{Config, run_import_depth_check, print_warnings_tree};

let mut cfg = Config {
    root: Some("./my-project".into()),
    threshold: 10,
    entry_glob: Some("src/**/*.ts".to_string()),
    tsconfig_paths: Default::default(),
};

cfg.initialize()?;
let warnings = run_import_depth_check(cfg.clone())?;

if !warnings.warnings.is_empty() {
    print_warnings_tree(&mut std::io::stdout(), &warnings.warnings, &cfg, cfg.threshold)?;
}
```

## License

MIT