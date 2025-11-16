# Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes.

## Installation

```bash
# Install oxiclean CLI
cargo install --path apps/oxiclean

# Or run from monorepo
just run-import-bloat --help
```

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean import-bloat

# Common options
oxiclean import-bloat --root ./my-project --threshold 100 --entry-glob "src/**/*.tsx"
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `--threshold <N>` - Max reachable modules before warning (default: 200)
- `--entry-glob <PATTERN>` - Glob pattern to filter entry files (default: all files in `/src/`)

## Example Output

```
⚠ Import bloat detected (threshold: 50 modules)

src/pages/Dashboard.tsx (127 modules)
├──  import './components' (89 modules)
└──  import '@/utils' (45 modules)

📊 Statistics:
  Total unique modules: 234
  ├── App code: 145
  ├── Workspace packages: 23
  └── External packages: 66
```

## How It Works

1. Parses JavaScript/TypeScript files using the fast OXC parser
2. Resolves all imports (static `import`, dynamic `import()`, and `require()`)
3. Builds a dependency graph of reachable modules
4. Reports files exceeding the threshold

**Features:**
- Resolves Node.js modules and TypeScript path mappings from `tsconfig.json`
- Handles static and dynamic imports
- Respects `.gitignore` patterns
- Skips test files

## Library Usage

```rust
use oxiclean_import_bloat::{Config, run_import_bloat_check, print_warnings_tree};

let cfg = Config {
    root: Some("./my-project".into()),
    threshold: 100,
    entry_glob: Some("src/**/*.ts".to_string()),
    ..Default::default()
};

let warnings = run_import_bloat_check(cfg.clone())?;
print_warnings_tree(&warnings, cfg.threshold);
```

## License

MIT
